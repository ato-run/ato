# Capsule v0.3 to v1 migration

Capsule v1 changes the resolved execution and Snapshot contracts without
silently reinterpreting existing artifacts. The v0.3 parser and deployed
Ready-State artifacts remain readable on their legacy paths.

## Manifest migration

The minimal v1 authoring surface is:

```toml
schema_version = "1"
name = "example"

[tools]
node = "22"

[[build.steps]]
command = ["npm", "ci"]

[[build.steps]]
command = ["npm", "run", "build"]

[run]
command = ["node", "dist/server.js"]

[web]
port = 8080
bind = "0.0.0.0"

[seal_at]
command = ["npm", "run", "verify-ready"]
timeout_seconds = 120

[state.data]
mount = "/data"
access = "read-write"
schema = "1"
snapshot = "exclude"
```

Commands are exact argv arrays. `seal_at.command` validates a disposable
restore of an immutable candidate; it does not identify the exact initial
capture instant. A custom readiness/gate DSL is not inferred from v0.3 fields.

## Lock migration

v1 adds a typed top-level `execution_contract` and its stored `execution_id` to
`ato.lock.json`. Both must be present together. The runtime rehashes the
contract on durable lock reads.

`execution_contract` and its derived `execution_id` are authenticated by the
lock identity/signature projection. Existing lock metadata,
provenance, diagnostics, evidence, timestamps, Snapshot references, runner
names, and Session facts remain outside the hash.

A v0.3 lock with neither v1 field remains a v0.3 lock; it is not promoted by
guessing. Explicit resolve/build finalizes runtime, dependency, filesystem, and
application output digests before writing the v1 pair.

## Snapshot migration

| Existing artifact | Capsule v1 behavior |
|---|---|
| `ato.ready-state/v1` without `execution_id` | inspectable; never eligible for exact v1 lookup |
| `ato.ready-state/v1` with a historical ID | inspectable; requires explicit re-resolution and migration |
| `ato.snapshot-manifest/v1` | requires valid `execution_id`, `snapshot_id`, and proven compatibility |

Migration writes a new immutable manifest and a new `snapshot_id`. It never
changes old bytes or treats the legacy Ready-State ID as the new Snapshot ID.
A v1 lock paired with only a legacy Ready-State manifest fails closed at run
time. Rebuilding is an intentional migration cost: `ato build` must perform the
disposable restore acceptance and persist `snapshot-manifest-v1.json` before
that artifact is eligible for local or Connected-Runner v1 restore.

Connected Runners keep a separate, explicit compatibility path for historical
`sha256:` execution IDs. That path verifies the lease ID, sealed backend facts,
and exact runner contract before restore. An execution ID in the v1 `blake3:`
namespace never uses this compatibility path and requires the authenticated v1
Snapshot manifest.

## Fail-closed compatibility errors

- `execution_contract` / `execution_id` pair incomplete
- unknown execution-contract field or unresolved required value
- stored `execution_id` mismatch
- missing or mismatched Snapshot `execution_id`
- unknown backend, VMM, kernel, CPU-template, codec, or runner compatibility
- running capture requested while live workload requires External State
- External State schema mismatch before read-write attach
- cold reconstruction output or policy mismatch
- workload-idle placeholder revocation not attested

These errors require explicit re-resolution, rebuild, migration, or policy
change. They never rewrite the old lock or issue a new identity implicitly.

See the
[accepted Capsule v1 model](rfcs/accepted/CAPSULE_V1_EXECUTION_MODEL_SPEC.md)
for normative details.
