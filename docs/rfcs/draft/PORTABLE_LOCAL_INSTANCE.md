# Portable local Application Instance

Status: draft implementation contract  
Date: 2026-09-17

## Context

`ato run file.capsule` is intentionally ephemeral: it validates one transport,
creates a temporary realization, verifies K, and leaves no durable author or
Instance state. A user also needs to import a portable Application once, stop
its current Run, and start the same local Instance again without mutating or
re-importing the source `.capsule`.

This is local runtime orchestration, not a new semantic root. ContractRef stays
the Capsule identity, DerivationRef stays the realization identity, and each
Run remains disposable execution evidence.

## Decision

Add a separate local lifecycle:

```text
immutable .capsule transport
          |
          v
Local Application record ---- immutable bundle bytes
          |
          v
Local Instance ------------- selected DerivationRef, bindings, snapshot ref
          |
          v
Run 1, Run 2, ... ---------- PID/process group, endpoint, receipt, logs
```

The first CLI surface is:

```text
ato app import <file.capsule> [--derivation <sha256:...>]
ato app start <instance-id> [--no-open] [--verification-receipt <path>]
ato app inspect <instance-id>
ato app stop <instance-id>
ato app export <instance-id> [--output <file.capsule>]
```

`ato run` keeps its existing ephemeral meaning. Import never starts a Run.
Starting always creates a new Run and verifies the original K before publishing
the Run as active. Stopping removes only active Run state; it does not delete
the Instance, its immutable bundle, or prior Run evidence.

## Storage and transaction boundaries

The local root is `$ATO_HOME`, or the existing platform default when that
variable is absent:

```text
apps/<application-id>/
  application.json
  bundles/<bundle-sha256>.capsule

instances/<instance-id>/
  instance.json
  active-run.json
  runs/<run-id>/
    output.log
    verification-receipt.json
```

Application IDs are derived from `ApplicationRef` only to choose a local
directory; they do not replace ContractRef. Different transport packings may
therefore share one Application directory while retaining separate immutable
bundle files. Every import creates an independent Instance even when its bytes,
K, ApplicationRef, and selected D match an earlier import.

Import validates the canonical bundle and selected D before writing durable
metadata. Metadata is canonical JCS. Immutable files use create-or-verify;
Instance and active-Run claims use create-new. A token fences activation and
release so a stale worker cannot publish or clear a newer Run.

## Runtime ownership and safety

The CLI starts a detached worker and exits only after the worker has:

1. re-read and digest-checked the stored bundle;
2. materialized a Run-specific workspace;
3. started the explicitly selected Adapter route;
4. fully satisfied the original K;
5. persisted the Run-scoped receipt; and
6. atomically published active Run metadata.

Stop validates boot-session identity, process start time, PID, and process
group before signalling the owned tree. PID alone is never authority. The
worker drops the Static/Process/OCI runtime and acknowledges cleanup before the
CLI terminates the supervisor and clears the token-fenced active record.

Portable v4 snapshot bundles are materialized into Instance-owned resource and
Asset namespaces, and `ato app export` writes the bundle currently selected by
the Instance. Library callers can seal JSON/browser-state resources and Assets
into a new K while preserving D. The exact object and verification rules are in
`PORTABLE_INSTANCE_SNAPSHOT.md`.

Browser flush/capture, runtime injection, Hosted restore, filesystem state,
and secret Binding remain later increments. Local unpacking alone must not be
described as a successful runtime restore.

## Testing strategy

- Store unit tests cover independent imports, explicit multi-D selection,
  immutable bundle tamper rejection, and one-active-Run token fencing.
- Process ownership tests prove current-process matching and stale start-time
  rejection.
- CLI integration performs import -> start -> inspect -> stop -> start ->
  inspect -> stop, checks distinct Run IDs, and checks that both receipts
  fully satisfy the same K and D without re-importing.
- Existing ephemeral Static, Process, OCI, export, and bundle-validation tests
  remain regression gates.

## Deferred work

This draft does not define browser-driven saved-data capture, field-aware Asset
alias resolution in opaque state, filesystem state mounts, portable Bindings,
User Runner placement, automatic planning, Instance deletion, or production
deployment. Snapshot-bearing export creates the separately specified
saved-state Contract; this local lifecycle never silently reuses K after user
data changes.
