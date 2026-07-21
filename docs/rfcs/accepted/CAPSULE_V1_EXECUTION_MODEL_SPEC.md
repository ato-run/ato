---
title: "Capsule v1 Execution Identity and Snapshot Model"
status: accepted
date: 2026-07-21
author: "@egamikohsuke"
ssot:
  - "crates/capsule/src/contract/execution_contract.rs"
  - "crates/capsule/src/engine/cold_reconstruction.rs"
  - "crates/snapshot/src/snapshot_manifest.rs"
  - "crates/snapshot/src/acceptance.rs"
  - "crates/snapshot/src/external_state.rs"
  - "crates/snapshot/src/workload_idle.rs"
related:
  - "../draft/CAPSULE_CORE_MODEL.md"
  - "../draft/beyond-reproducible-build.md"
  - "../draft/HASH_AND_PROVENANCE_POLICY.md"
  - "../archived/EXECUTION_IDENTITY_SPEC.md"
  - "../accepted/ADR-002-signature-format-jcs.md"
  - "../../execution-identity.md"
  - "../../snapshot.md"
  - "../../snapshot-v1-compatibility.md"
---

# Capsule v1 Execution Identity and Snapshot Model

**Tracking issue:** [#1086](https://github.com/ato-run/ato/issues/1086)

## 1. Overview

This specification defines the execution model for the future
`capsule.toml` schema v1. It does not describe the currently implemented
`capsule.toml` v0.3 schema.

Capsule v1 has one exact execution identity:

> **Execution Identity identifies a resolved, target-specific launch envelope.**

A Snapshot is not another execution identity. It is an immutable cache that is
subordinate to exactly one Execution Identity. External State is attached at
run time, and a Session records one concrete run.

The complete user-facing model has five elements:

```text
1. capsule.toml
   Author intent and the recipe for constructing an execution environment.

2. Execution Identity
   The exact, resolved launch contract for one target.

3. Snapshot
   An immutable cache of booted state for one Execution Identity.

4. External State
   User data, secrets, identity, and concrete service bindings.

5. Session
   One running instance and its Receipt.
```

The governing sentence is:

> Ato resolves `capsule.toml` into a resolved-target-specific Execution
> Identity and may save its booted state as a Snapshot. `ato run` restores a
> compatible Snapshot or cold-reconstructs the same execution contract,
> attaches External State, and starts a Session.

## 2. Status and scope

This document is the accepted design authority for the Capsule v1 execution
model. The accepted v0.3 Capsule specification remains authoritative for the
currently shipped `capsule.toml` parser until the v1 authoring schema is wired
into the CLI. Legacy Ready-State artifacts remain governed by their existing
wire contract and the migration rules in section 16.3.

### 2.1 In scope

- The five-element Capsule v1 execution model.
- The sole exact execution identity and its canonical hash boundary.
- The role of `ato.lock.json` as a serialization of the resolved contract.
- The minimum Capsule v1 authoring surface needed by this model.
- Snapshot identity, compatibility, selection, capture, and acceptance.
- External State boundaries and Session Receipt requirements.
- Cold reconstruction and fail-closed digest verification.
- Migration rules from current multi-identity and Snapshot-derived models.

### 2.2 Out of scope

- A complete JSON Schema for every future `capsule.toml` v1 feature.
- Backend-specific Firecracker, OCI, filesystem, or VMM implementation details.
- Deployment, placement, scheduling, billing, and registry product policy.
- A general HTTP/readiness/gate DSL.
- Deterministic instruction, syscall, scheduler, clock, or network replay.
- GPU state capture.
- User-facing Materialization identifiers.

## 3. Core model

### 3.1 Data flow

```text
capsule.toml
    | resolve / build
    v
ato.lock.json
  resolved execution contract
    | canonical hash
    v
Execution Identity
    |\
    | \-- cold reconstruction
    |
    +---- Snapshot cache
              |
              + External State
              v
           Session
              |
              v
           Receipt
```

The cardinalities are:

```text
Capsule            1 --- N Execution Identities
Execution Identity 1 --- N Snapshots
Execution Identity 1 --- N Sessions
Session            N --- N External State references
```

`Capsule` is the user-facing grouping for an app, tool, or service. Registry
names, versions, URLs, and target labels resolve to a concrete
`execution_id`; Capsule itself has no second exact execution identity.

Example:

```text
Capsule: example
  +-- Execution Identity: linux-x86_64-gnu
  +-- Execution Identity: linux-aarch64-gnu
  +-- Execution Identity: windows-x86_64-msvc
```

### 3.2 Prohibited public identity proliferation

Capsule v1 MUST NOT introduce any of the following as another exact public
execution identity:

- Capsule Revision identity
- Resolved Capsule Revision identity
- ExecutionPlan identity
- LaunchRecord identity
- Realization identity
- Materialization identity

The implementation MAY use plans, launch records, realizations, or
materializations as internal data structures. They MUST NOT compete with
`execution_id` as the answer to “which exact execution contract is this?”

### 3.3 Internal Materialization term

The runtime filesystem, dependency output, OCI image, application build output,
and VM rootfs are not Snapshots. If the implementation needs a common term for
them, it MAY call them **Materializations** internally:

```text
Execution Identity 1 --- N Materializations
```

`Materialization` is not part of the v1 authoring schema or required user-facing
vocabulary.

## 4. Execution Identity

### 4.1 Definition

Execution Identity is a content-addressed identity of the resolved launch
envelope. It identifies what the process is intended to observe at launch. It
is not an identity of a Snapshot, build job, runner, machine, or Session.

An Execution Identity is specific to a **resolved target**, including concrete:

- operating system
- architecture
- ABI and libc contract
- runtime artifact digest
- dependency output digest
- application build output digest, when a build output exists

Linux x86_64 and Linux arm64 therefore produce different execution IDs even
when they originate from the same Capsule and source revision.

### 4.2 Identity-bearing contract

The v1 execution contract MUST include every resolved condition that can change
the launch envelope:

| Facet | Required identity-bearing content |
|---|---|
| Source | Materialized source digest and source projection rules |
| Target | OS, architecture, ABI/libc, and observable target features |
| Runtime | Resolved runtime artifact and dynamic runtime contract |
| Dependencies | Derivation identity and actual immutable output identity |
| Build outputs | Actual immutable output digest and projection |
| Launch | Entrypoint, exact argv, cwd, and process model |
| Environment | Non-secret values, variable requirements, normalization, and inheritance policy |
| Filesystem | Immutable layers, mount topology, access modes, and writable-boundary contracts |
| Network | Effective ingress, egress, DNS, and isolation policy |
| Capabilities | Effective filesystem, host, device, and sandbox policy |
| Surface | Declared guest bind address, protocol, and guest port |
| External State schema | Binding name, mount/injection contract, access mode, schema identity, and Snapshot exclusion rule |

An authored version selector such as `node = "22"` is not sufficient by
itself. The identity-bearing contract records the exact resolved runtime
artifact and its digest.

### 4.3 Conditions excluded from identity

The following are concrete execution records or infrastructure facts and MUST
NOT change `execution_id` unless they become application-observable launch
conditions:

- runner name or runner database ID
- provider name
- machine ID
- build job ID
- Session ID
- Snapshot ID or Snapshot format
- host-assigned IP address
- dynamically allocated host port
- creation timestamp
- resolver logs and diagnostic messages
- provenance display URLs
- actual External State owner, volume ID, generation, or data content
- secret, token, credential, or identity values

The declared guest port and network policy are identity-bearing; the dynamic
host port chosen to expose a Session is not.

### 4.4 Observable target features versus Snapshot compatibility

Target facts belong to one of two boundaries:

```text
Execution Identity
  Facts observable by the application or required by its launch contract.

Snapshot compatibility
  Additional physical constraints required only to restore cached state.
```

For example, an application-required CPU instruction set, GPU driver contract,
or kernel feature belongs to Execution Identity. A Firecracker snapshot format,
VMM version, CPU template required only for restore, or runner restore contract
belongs to Snapshot compatibility.

If changing a fact can change application-visible behavior, the safe default is
to place it in Execution Identity.

### 4.5 Canonicalization and hash

Only the identity-bearing resolved contract is hashed:

```text
execution_id =
  "blake3:" + hex(
    BLAKE3(
      UTF8("ato.execution-contract/v1")
      || 0x00
      || JCS(lock.execution_contract)
    )
  )
```

Requirements:

- `ato.execution-contract/v1` is the domain separator and contract version.
- JCS canonicalization MUST use the project-pinned implementation and test
  vectors.
- The hash input MUST NOT contain `execution_id` itself.
- A semantic change to canonicalization or the identity-bearing field set MUST
  introduce a new contract version.
- Reordering TOML keys, adding diagnostics, or changing provenance-only fields
  MUST NOT change the digest.

BLAKE3, JCS, the domain separator, and the identity-bearing projection boundary
are normative for `ato.execution-contract/v1`.

### 4.6 Build outputs and identity finalization

Some identity-bearing values, especially dependency and application build
output digests, are known only after build. The build flow therefore finalizes
Execution Identity after immutable output observation but before Snapshot
selection or publication:

```text
resolve declared inputs
  -> build dependencies and application outputs
  -> compute actual output digests
  -> finalize lock.execution_contract
  -> compute execution_id
  -> optionally capture and accept a Snapshot
```

Snapshot layer IDs are not folded into `execution_id`. A different Snapshot of
the same launch contract remains subordinate to the same Execution Identity.

## 5. `ato.lock.json`

`ato.lock.json` is not a sixth domain concept and is not hashed as an opaque
file. It is the serialization boundary for a resolved Execution Identity plus
non-identity metadata.

Conceptually:

```json
{
  "schema": "ato.lock/v1",
  "execution_contract": {
    "schema": "ato.execution-contract/v1",
    "source": {},
    "target": {},
    "runtime": {},
    "dependencies": {},
    "build_outputs": {},
    "launch": {},
    "environment": {},
    "filesystem": {},
    "network_policy": {},
    "capability_policy": {},
    "external_state_schema": {}
  },
  "execution_id": "blake3:...",
  "provenance": {},
  "diagnostics": {},
  "evidence": {},
  "generated_at": "..."
}
```

The producer MUST verify on read that the stored `execution_id` matches the
canonical hash of `execution_contract`. Provenance, evidence, timestamps, and
diagnostics are preserved for audit and debugging but are outside the identity
projection.

## 6. Minimum `capsule.toml` v1 authoring surface

The v1 authoring model declares intent and uses exact argument vectors. It does
not require an Ato-specific HTTP check or lifecycle-gate DSL.

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
snapshot = "exclude"
schema = "1"
```

### 6.1 Command rules

- `build.steps[].command`, `run.command`, and `seal_at.command` are argv arrays.
- They execute without an implicit shell.
- An implementation MUST preserve argument boundaries exactly.
- Shell behavior is available only through an explicitly selected shell in the
  argv, for example `["sh", "-lc", "..."]`.
- `seal_at.timeout_seconds` MUST be positive and bounded by platform policy.

### 6.2 Surface rules

`web.port` and `web.bind` describe the guest-visible launch contract and are
identity-bearing. Host exposure, public URL, host port, and proxy assignment are
Session facts and are recorded in the Receipt.

### 6.3 `seal_at` rules

`seal_at.command` is an arbitrary Capsule-authored verification program. It may
perform an HTTP request, an API workflow, browser automation, database
initialization checks, or another application-specific verification.

`seal_at.command` is evaluated against a disposable restore of an immutable
candidate Snapshot. It determines whether that candidate may be accepted; it
does not define the exact instant when the builder captures its first candidate.

Ato interprets only its process result:

- exit code 0 means the candidate passed
- any other exit status means failure
- timeout means failure
- timeout termination MUST kill the full verification process tree

No Ato-specific HTTP, gate, readiness-level, or publish-at DSL is required.

## 7. Snapshot

### 7.1 Definition

> A Snapshot is an immutable cache of a booted environment state, subordinate
> to one Execution Identity, that accelerates restore on a compatible runner.

The workload may be running or workload-idle according to the capture policy;
the guest/runtime environment itself has already been booted and prepared.

A Snapshot MUST reference exactly one `execution_id`:

```text
snapshot.execution_id = execution_id
```

Multiple Snapshots MAY exist for one Execution Identity because capture time,
backend format, runner restore constraints, compression, memory layout, or
optimization profiles may differ.

### 7.2 What is not a Snapshot

The following are immutable build or runtime objects, but are not Snapshots:

- runtime filesystem
- dependency output
- OCI image
- application build output
- generic filesystem artifact

They MAY be managed as internal Materializations.

### 7.3 Snapshot manifest

The conceptual v1 Snapshot Manifest contains:

```text
SnapshotManifest
  schema = "ato.snapshot-manifest/v1"
  execution_id                  required
  compatibility_contract
  memory_layer_refs
  vmstate_layer_refs
  disk_layer_refs
  restore_contract
  capture_policy
  capture_provenance
  sanitization_attestation
  secret_scan_attestation
```

`snapshot_id` is the content address of the canonical manifest payload:

```text
snapshot_id =
  "blake3:" + hex(
    BLAKE3(
      UTF8("ato.snapshot-manifest/v1")
      || 0x00
      || JCS(snapshot_manifest_without_snapshot_id)
    )
  )
```

To avoid self-reference, `snapshot_id` MUST NOT be a field in the hashed
payload. An API or registry MAY return it in an outer envelope.

### 7.4 Compatibility contract

Snapshot compatibility contains restore-only constraints, including as
applicable:

- snapshot backend and format version
- VMM and state codec contract
- guest kernel identity
- CPU template or restore feature set
- runner restore contract
- portability tier

Unknown compatibility is not compatibility. Selection and restore MUST fail
closed when the runner cannot prove that it satisfies the Snapshot contract.

### 7.5 Selection

Snapshot lookup is:

```text
execution_id
  + snapshot backend compatibility
```

Lookup MUST NOT select by Capsule name, target label, source commit,
`capsule_manifest_hash`, newest creation time, or runner name without first
requiring exact `execution_id` equality.

Selection MAY rank compatible candidates by restore cost, locality, hotset,
format preference, or age after the exact identity and compatibility filters.

### 7.6 Immutability and acceptance

Every captured candidate is immutable. Acceptance is registry metadata, not a
mutation of the Snapshot:

```text
candidate -> accepted | rejected | quarantined
```

An accepted Snapshot that later fails integrity or compatibility checks MUST be
quarantined. It MUST NOT be silently repaired under the same `snapshot_id`.

## 8. Snapshot capture and acceptance

### 8.1 Validation must not contaminate the accepted Snapshot

The accepted Snapshot is captured before validation. Validation runs against a
disposable restore:

```text
launch build guest
  -> capture immutable candidate Snapshot
  -> restore candidate into a disposable Session
  -> run seal_at.command
  -> exit 0: mark candidate accepted
  -> otherwise: reject candidate
  -> always destroy verification overlay and Session
```

This ordering prevents API calls, browser tests, test users, database writes,
and other validation effects from entering the accepted Snapshot.

### 8.2 Candidate timing and retries

The builder MAY capture another candidate after a failed validation while the
build guest continues to run:

```text
capture candidate
  -> disposable restore / verify fails
  -> continue build guest
  -> wait according to builder policy
  -> capture a new candidate
```

Retries MUST be bounded by a build deadline and maximum attempt count. Retry
intervals, attempt limits, and scheduler policy are implementation details and
do not affect Execution Identity.

### 8.3 Capture policies

The Snapshot manifest records one of two internal capture policies:

```text
running
  The workload requires no External State to be live.
  The workload remains running in the captured candidate.

workload_idle
  The workload requires External State or restore-time bindings.
  The workload is stopped and synthetic placeholders are revoked before
  capture. Restore attaches bindings and starts the workload.
```

For `workload_idle`, any preparation with synthetic bindings MUST complete
before capture, and the builder MUST stop the workload and revoke placeholders
before capturing the candidate. Disposable validation may attach only
ephemeral synthetic bindings that conform to the declared External State
schema.

This policy is an internal Snapshot restore contract, not a new public identity.

The final v1 architecture defines both policies, but the first implementation
slice supports `running` only. `workload_idle` is an independent lifecycle
follow-up because it requires workload stop, placeholder revocation,
restore-time binding delivery, restart ordering, and failure cleanup. Until that
follow-up lands, a Snapshot build that requires External State for a live
workload MUST fail closed as ineligible; it MUST NOT fall back to a
secret-bearing running capture.

### 8.4 Validation environment

Snapshot acceptance verification MUST satisfy all of the following:

- no production secret is connected
- no user-owned state is connected
- no Ato user identity is connected
- synthetic identities and bindings are explicitly validation-only
- verification runs in the restored guest/runtime boundary
- the verification overlay is always destroyed
- failure and timeout terminate the verification process tree

### 8.5 Attestations, not proof of absence

The Snapshot manifest MUST NOT claim a general “no-secret proof.” Complete proof
that arbitrary bytes contain no secret is not available.

The primary guarantee is structural:

- production secrets are never connected before capture
- Ato identity is never connected before capture
- user state is never connected before capture
- excluded state paths are backed by separate volumes

The manifest may additionally carry:

- `sanitization_attestation`: which structural cleanup and revocation steps ran
- `secret_scan_attestation`: scanner identity, policy, coverage, and redacted
  result
- `capture_policy`: whether the workload was running or idle at capture

Secret scanning is defense in depth and MUST NOT be described as proof of
absence.

## 9. External State

### 9.1 Definition

External State is mutable or principal-specific state attached to a Session:

- user data
- persistent application data
- secret and API key values
- OAuth tokens
- Ato identity
- concrete database connection information
- concrete service bindings

Shared Snapshots MUST NOT contain these values.

### 9.2 Identity-bearing schema versus runtime value

Execution Identity includes the External State contract:

- binding/state name
- mount path or injection target
- access mode
- schema identity
- Snapshot exclusion contract

Execution Identity excludes the attached instance:

- owner ID
- volume or binding instance ID
- data bytes
- current state generation
- secret value
- identity assertion value

Example:

```toml
[state.data]
mount = "/data"
snapshot = "exclude"
schema = "1"
```

The contract means:

- `/data` is a separate writable boundary, not part of shared Snapshot layers
- build and acceptance use an empty or synthetic ephemeral volume
- run attaches a compatible External State instance
- incompatible schema fails before read-write attach

### 9.3 Receipt boundary

The Session Receipt records which concrete state was attached without placing
its contents or secret values in the Receipt:

```text
execution contract:
  state data at /data, schema 1, read-write, excluded from Snapshot

Session Receipt:
  state_ref = opaque:user-state-ref
  state_generation = gen_456
```

State generation changes do not change `execution_id`.

## 10. Commands

### 10.1 `ato build`

```text
read capsule.toml
  -> resolve target, runtime, and dependencies
  -> build dependency and application outputs
  -> compute actual immutable output digests
  -> finalize ato.lock.json execution_contract
  -> compute Execution Identity
  -> optionally launch and capture candidate Snapshots
  -> validate candidates through disposable restores
  -> return Execution Identity and any accepted Snapshot
```

`ato build` MUST return a valid Execution Identity even when Snapshot creation
is unsupported or no candidate is accepted.

An implementation MUST NOT publish a shared Snapshot without a
Capsule-authored `seal_at` contract or an explicitly trusted platform-supplied
equivalent.

### 10.2 `ato run`

```text
resolve or load Execution Identity
  -> find Snapshots with exact execution_id
  -> filter by proven backend compatibility
  -> compatible Snapshot exists: restore it
  -> otherwise: cold-reconstruct the execution contract if policy allows
  -> attach compatible External State
  -> start or resume the workload according to capture_policy
  -> begin Session
  -> emit Receipt
```

Production and Store policy MAY forbid cold build or cold reconstruction. That
policy controls whether a run is allowed; it does not create another execution
identity.

## 11. Cold reconstruction

When no compatible Snapshot exists, Ato reconstructs the resolved execution
contract and verifies its immutable outputs:

```text
cold reconstruct
  -> runtime digest matches
  -> dependency output digest matches
  -> application build output digest matches
  -> launch envelope matches
  -> attach External State
  -> start Session
```

If any identity-bearing output differs, Ato MUST fail closed. It MUST NOT launch
the differing output under the existing `execution_id`.

A new Execution Identity may be issued only after an explicit re-resolution or
rebuild finalizes a new resolved contract.

Cold reconstruction promises launch-contract equivalence, not identical memory
bytes, instruction traces, timing, scheduler behavior, or external network
responses.

## 12. Session and Receipt

A Session is one concrete run. It has `session_id`, but no new exact execution
identity.

The Receipt MUST record at least:

```text
session_id
execution_id
launch_mode                 snapshot | cold
selected_snapshot_id        absent for cold launch
runner and provider facts
snapshot compatibility decision
guest surface and assigned host endpoints
attached External State refs and generations
started_at
stopped_at
exit or teardown result
```

Runner, provider, dynamic endpoint, External State generation, and Snapshot
choice are recorded facts. They do not alter Execution Identity unless a fact
was part of the resolved application-visible target contract.

Receipts MUST redact secret values and identity assertions.

## 13. Normative decisions

Capsule v1 fixes exactly three architecture decisions:

1. **Execution Identity is the sole exact execution identity and includes the
   resolved target.**
2. **A Snapshot is an immutable cache subordinate to exactly one Execution
   Identity.**
3. **Snapshot acceptance is verified in a disposable restored Session;
   validation effects never enter the accepted Snapshot.**

These decisions are normative even if the implementation later introduces new
internal plans, records, or materialization types.

## 14. Security requirements

- Execution contract parsing and canonicalization fail closed on unknown or
  ambiguous identity-bearing fields.
- Snapshot restore requires exact `execution_id` equality and proven backend
  compatibility.
- External State values never enter a shared Snapshot.
- Secret values and identity assertions never enter lockfiles or Receipts.
- Snapshot acceptance never uses a production secret, user state, or user
  identity.
- Snapshot scanners emit redacted findings and are described as attestations,
  not proofs.
- Snapshot candidate and accepted bytes are immutable and content-addressed.
- Cold reconstruction rejects every identity-bearing digest mismatch.
- Actual filesystem mount enforcement MUST match the identity-bearing
  filesystem contract.

## 15. Required invariants

1. Execution Identity is the sole exact identity of a launch contract.
2. Execution Identity is specific to a resolved target.
3. Snapshot is subordinate to exactly one Execution Identity.
4. Snapshot format and Snapshot ID do not affect Execution Identity.
5. User data, production secrets, and identity are absent from shared Snapshot
   capture by construction.
6. A Snapshot is selected only after exact `execution_id` equality and proven
   compatibility.
7. Snapshot absence does not redefine the execution contract.
8. Cold reconstruction verifies all resolved runtime, dependency, and build
   output digests before launch.
9. External State schema is identity-bearing; concrete state and values are not.
10. Runtime-specific differences are recorded in the Session Receipt rather
    than promoted to new identity types.
11. Validation effects occur only in a disposable Session and overlay.
12. `ato.lock.json` metadata outside `execution_contract` does not affect
    `execution_id`.

## 16. Compatibility and migration

### 16.1 Existing Execution Identity documentation

The current graph model describes declared, resolved, and observed execution
identity domains. Capsule v1 collapses the public exact identity to one resolved
Execution Identity. Declared intent remains `capsule.toml`; observations remain
Receipt evidence. Neither receives another public exact execution ID.

Internal graph projections MAY remain if useful, but only the resolved target
contract produces the v1 public `execution_id`.

### 16.2 Superseded Snapshot-derived draft

The archived
[Execution Identity Spec](../archived/EXECUTION_IDENTITY_SPEC.md) defined
`execution_id` from post-seal Snapshot layer CAS IDs and runner-class facets.
This specification supersedes that model:

- runtime, dependency, and application output digests remain identity-bearing
- Snapshot memory/vmstate/disk layer IDs are excluded
- Snapshot compatibility is separate from Execution Identity
- `execution_id` is finalized before Snapshot capture and selection

### 16.3 Current Ready-State manifest

The current `ato.ready-state/v1` `ReadyStateManifest` carries both
`capsule_manifest_hash` and an optional `execution_id`. Capsule v1 introduces a
new `ato.snapshot-manifest/v1` wire schema rather than making a field required
inside the legacy schema:

- legacy `ato.ready-state/v1` Snapshots remain deserializable for inspection and
  explicit migration, including artifacts without `execution_id`
- legacy Snapshots without `execution_id` are never eligible for v1 exact lookup
- `execution_id` is required by the `ato.snapshot-manifest/v1` type and validator
- `capsule_manifest_hash` is removed from compatibility selection and may remain
  only as capture provenance
- `runner_class_id` is replaced or generalized by the compatibility contract
- `no_secret_proof` is replaced by structural capture policy plus sanitization
  and secret-scan attestations
- Registry lookup requires exact `execution_id` before compatibility ranking

The runtime MUST NOT silently reinterpret a legacy manifest as a v1 manifest.
Migration creates a new immutable manifest and therefore a new `snapshot_id`.

### 16.4 Deprecated external vocabulary

The following terms may remain in legacy APIs or internal implementation while
migration is in progress, but new v1 user-facing documentation MUST NOT depend
on them as identity concepts:

- Capsule Revision identity
- Resolved Capsule Revision identity
- ExecutionPlan identity
- LaunchRecord identity
- Realization identity
- Materialization identity
- readiness levels and multiple lifecycle gate identities

## 17. Test strategy

### 17.1 Canonicalization mutation matrix

Tests MUST prove that each identity-bearing mutation changes `execution_id`:

- source digest
- target OS, architecture, ABI, or libc
- runtime artifact digest
- dependency derivation or output digest
- application build output digest
- entrypoint, argv, or cwd
- non-secret environment value or inheritance policy
- filesystem mount topology or access mode
- network or capability policy
- External State name, schema, mount, access, or exclusion contract

Tests MUST prove that excluded mutations do not change `execution_id`:

- lockfile timestamp
- diagnostic message or resolver log
- builder, runner, provider, or machine name
- Snapshot ID or format
- Session ID
- dynamic host IP or port
- provenance display URL
- secret value
- External State owner, volume ID, generation, or content

### 17.2 Snapshot selection and restore

- exact `execution_id` mismatch is rejected
- missing compatibility facts are rejected
- incompatible backend, kernel, codec, or CPU template is rejected
- compatible candidates may be ranked only after identity and compatibility
  filtering
- quarantined Snapshots are never selected

### 17.3 Capture and acceptance

- validation mutations never appear in accepted Snapshot bytes
- validation overlays are removed on success, failure, and timeout
- timeout kills the full process tree
- retries are bounded
- `running` captures contain no required External State
- `workload_idle` captures stop the workload and revoke placeholders
- scanners emit attestations without claiming proof of absence

### 17.4 External State

- excluded mount bytes are absent from every shared Snapshot layer
- compatible schema attaches successfully
- incompatible schema fails before read-write attach
- changing state generation does not change `execution_id`
- Receipt records opaque state reference and generation without data or secrets

### 17.5 Cold reconstruction

- matching output digests allow launch
- runtime, dependency, or application output mismatch fails closed
- mismatch never silently updates the existing lock or Execution Identity
- an explicit re-resolution can produce a new Execution Identity

## 18. Known limitations

- The full `capsule.toml` v1 schema is not defined here; this document fixes the
  execution identity, Snapshot, state, and lifecycle boundaries it must obey.
- Some applications cannot produce a useful running-process Snapshot without
  real External State. They use `workload_idle` or cold launch.
- Secret scanning cannot guarantee absence of every possible secret pattern.
- Observable CPU, GPU, kernel, and driver boundaries require conservative
  platform-specific classification.
- Cross-run behavioral determinism is not guaranteed for time-bound,
  network-bound, or externally stateful applications.

## References

- [Beyond Reproducible Builds](../draft/beyond-reproducible-build.md) — launch-envelope
  definition and separation of dependency derivation from output identity.
- [Capsule Core Model](../draft/CAPSULE_CORE_MODEL.md) — broader Capsule declaration,
  composition, placement, and install model under revision.
- [Hash and Provenance Policy](../draft/HASH_AND_PROVENANCE_POLICY.md) — project hash
  domains and provenance separation.
- [ADR-002: Signature Format JCS](ADR-002-signature-format-jcs.md) —
  canonical JSON precedent.
- [Execution Identity](../../execution-identity.md) — public identity guide.
- [Snapshot](../../snapshot.md) — public Snapshot and lifecycle guide.
- [Snapshot v1 Compatibility](../../snapshot-v1-compatibility.md) — deployed
  Ready-State supported-surface contract and v1 migration context.
- `crates/capsule/src/contract/execution_contract.rs` — canonical resolved
  contract and `execution_id` implementation.
- `crates/capsule/src/engine/cold_reconstruction.rs` — fail-closed cold path.
- `crates/snapshot/src/snapshot_manifest.rs` — Capsule v1 manifest, migration,
  selection, and quarantine.
- `crates/snapshot/src/acceptance.rs` — disposable acceptance orchestration.
- `crates/snapshot/src/external_state.rs` — state exclusion and schema gate.
- `crates/snapshot/src/workload_idle.rs` — stopped-workload capture and restore
  ordering.
