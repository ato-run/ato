# Capsule v0.3 to v1 migration

> **Superseded.** This document describes the removed pre-Computation
> application execution architecture and is retained only as migration
> history. Current normative behavior is defined by the accepted Computation,
> Repository Workspace, Object Bundle, and Provider Materialization RFCs.

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
the canonical lock, **`capsule.lock`**. Both must be present together. The
runtime rehashes the contract on durable lock reads.

`capsule.lock` is UTF-8 canonical JSON despite the absent `.json` suffix. The
older name `ato.lock.json` is legacy-only: it is still read, never written, and
never promoted by guessing. A tree carrying **both** `capsule.lock` and
`ato.lock.json` fails closed rather than picking one — resolve it by deleting
the legacy file once you have confirmed the canonical lock is the one you want.
See [the archived model's §5](rfcs/archived/CAPSULE_V1_EXECUTION_MODEL_SPEC.md) for the
full precedence table, including the separate `capsule.lock.json` compatibility
lock.

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
| `ato.snapshot-manifest/v1` | requires valid `execution_id`, `snapshot_id`, authenticated Artifact Envelope, and proven compatibility |

Migration writes a new immutable manifest and a new `snapshot_id`. It never
changes old bytes or treats the legacy Ready-State ID as the new Snapshot ID.
A v1 lock paired with only a legacy Ready-State manifest fails closed at run
time. Rebuilding is an intentional migration cost: `ato build` must perform the
disposable restore acceptance and persist `snapshot-manifest-v1.json` plus its
content-addressed `artifact-envelope-v1.json` before that artifact is eligible
for local or Connected-Runner v1 restore.

Schema fields, not hash prefixes, choose the migration path. A lease declaring
`execution_identity_schema = "ato.execution-contract/v1"` must also carry the
expected Snapshot manifest schema/ID and Artifact Envelope schema/ID. A legacy
BLAKE3 ID without this schema remains legacy and is never implicitly promoted.
The legacy path still verifies the lease ID, sealed backend facts, and exact
runner contract before restore.

Local v1 publication uses
`snapshots/<execution_id>/<snapshot_id>/`; the old
`ready-state/<capsule_manifest_hash>/` directory remains legacy-only. Rebuilding
the same Capsule manifest for a new resolved target therefore retains the old
Execution Identity and Snapshot instead of overwriting its sidecar.

Local acceptance uses one immutable receipt per Snapshot under the Execution
Identity directory. Each receipt authenticates the Snapshot, Envelope, and
disposable-restore acceptance receipt IDs through a caller-authenticating,
privilege-separated helper configured by
`ATO_SNAPSHOT_ACCEPTANCE_SIGNER_HELPER`. This value locates the helper and is not
a key: Ato sends a canonical projection and receives only `key_id` plus an
opaque authenticator. The helper owns key generation, protected persistence,
and rotation; historical key IDs remain verification-only until explicitly
revoked. Ato never accepts key bytes through its environment, and all
capsule-controlled install/build/probe children strip the complete
`ATO_SNAPSHOT_ACCEPTANCE_*` namespace. Missing helper configuration, an unknown
key ID, or a failed verification fails publication and selection closed.

## Fail-closed compatibility errors

- `execution_contract` / `execution_id` pair incomplete
- unknown execution-contract field or unresolved required value
- stored `execution_id` mismatch
- missing or mismatched Snapshot `execution_id`
- partial or mismatched execution/Snapshot/Envelope schema metadata
- sidecar, Envelope, acceptance receipt, or CAS-root mismatch
- unknown backend, VMM, kernel, CPU-template, codec, or runner compatibility
- running capture requested while live workload requires External State
- External State schema mismatch before read-write attach
- cold reconstruction output or policy mismatch
- workload-idle placeholder revocation not attested

These errors require explicit re-resolution, rebuild, migration, or policy
change. They never rewrite the old lock or issue a new identity implicitly.

See the
[archived Capsule v1 model](rfcs/archived/CAPSULE_V1_EXECUTION_MODEL_SPEC.md)
for normative details.
