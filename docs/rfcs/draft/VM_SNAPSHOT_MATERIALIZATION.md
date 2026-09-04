# Generic VM Snapshot Materialization

Status: Draft

## Decision boundary

`ato.materialize.vm.snapshot@1` is a restore-capable physical
Materializer for an already-existing `ComputationRef`. Firecracker is one
backend implementation; it is not the public semantic identity.

The following identities remain independent:

```text
target ComputationRef
!= RecordFrontier reference
!= VM descriptor reference
!= VM artifact/chunk digest
!= backend or capture provenance
```

`ato.snapshot@1` remains a legacy compatibility reader. New workspace
snapshots use `ato.materialize.workspace.snapshot@1`.

## Descriptor

The canonical descriptor records:

- `target_computation_ref`, resolved as an existing computation;
- optional capture provenance `record_frontier_ref`, required by version 1;
- backend and snapshot format;
- architecture and `guest_os`;
- `host_backend_contract`, CPU, memory, device, network, and vsock contracts;
- the Firecracker version when the selected backend is Firecracker;
- role-addressed `memory`, `rootfs`, `vmstate`, and optional metadata artifacts;
- contiguous chunks with ordinal, offset, length, and content reference;
- extension-defined Contract descriptors and state contract references;
- capture provenance proving a completed barrier, quiesced source
  Realization, and source Realization identity.

Runner class is not canonical identity. Compatibility is computed from
physical `RunnerCapabilities` and is fail-closed for unknown architecture,
guest OS, backend/version, format, CPU, memory, device, network, or vsock
facts.

## Capture

Capture accepts only a known target computation and an active VM Realization
registered with the backend:

```text
known ComputationRef + active Realization
  -> Capture Barrier
  -> sealed RecordFrontier verification
  -> Realization quiesce
  -> backend physical capture
  -> chunked descriptor associated with the same ComputationRef
```

The Record Writer verifier is injected by the product layer. The generic
Materializer does not depend on Record Writer storage internals and never asks
the writer to advance a computation.

The backend must echo the exact target and frontier references. VM bytes and
frontier bytes are never inputs to computation identity.

## Restore and planning

`RealizationPlanner` evaluates Materializer compatibility, provisionable
Actuator routes, Contract verifier availability, port feasibility, runner
capabilities, placement, and trust policy. Its explicit preference is local VM,
local reconstruction/replay, hosted VM, then hosted reconstruction/replay.

The selected Materializer performs exactly one path. A VM backend never falls
back to workspace, source, OCI, or replay. Player dispatch remains confined to
a selected replay path.

Restore reconstructs role artifacts into a per-session workspace, acquires a
runner slot, provisions TAP/vsock paths, starts Firecracker, and loads the
snapshot. The returned `Realization` supports hidden activation, publication,
wait, and quiesce. Process, API socket, TAP, vsock path, temporary session, and
slot are owned by cleanup guards on every failure and drop path.

External Surface publication occurs only after extension Contracts pass.

## Firecracker backend contract

The current backend probes Linux/KVM, the exact Firecracker version, `ip`
support, CPU flags, memory, snapshot format, content-addressed rootfs path,
TAP, and vsock UDS support. Non-Linux hosts and incomplete probes expose no VM
compatibility.

The low-level process, Unix HTTP API, snapshot-load, and cleanup mechanics are
adapted from the historical Ready-State implementation. Legacy Store identity,
Snapshot IDs, launch APIs, and D1 rows are not reused.

## Legacy artifacts

`fc-full-file-v1` integrity receipts prove only digest/length closure of the
old physical graph. They do not prove a current computation, license,
private-data absence, compatibility, or restore acceptance. Migration must
reconstruct from pinned current source/OCI, replay applicable Records, pass
Contracts, and capture a new VM Materialization for a known computation.

## Deferred work

- canonical Computation residual identity is tracked separately in
  `COMPUTATION_RESIDUAL_IDENTITY_REDESIGN.md`;
- content-addressed network transport is a separate client/server PR stack;
- an actual Linux/KVM runner capture and three fresh restores are staging
  acceptance gates, not claims established by local fake-backend tests.

## Non-goals

- making VM state a Semantic Core primitive;
- deriving a computation from VM bytes, Records, or RecordFrontier;
- restoring legacy Store/launch abstractions;
- app-specific VM Materializers;
- hidden Materializer fallback;
- publishing Source/OCI/replay as though it were a VM restore.
