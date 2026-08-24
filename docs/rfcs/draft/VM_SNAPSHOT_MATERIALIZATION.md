# VM Snapshot Materialization

Status: Draft

## Problem

Historical ato.run Ready-State artifacts are Firecracker VM realizations. They
contain a content-addressed rootfs, memory image, VM state, an exact runner
class, and a restore contract. The current accepted Ato contract intentionally
does not claim to restore them:

- `ato.snapshot@1` is a verify-only workspace/filesystem artifact;
- VM snapshot restoration is model/future;
- Capsule Bundle v2 is a self-contained closure, while historical VM artifact
  graphs are hundreds of MiB and the current ato.run Bundle endpoint accepts at
  most 32 MiB.

Treating an old Snapshot ID as a Capsule or labeling the old manifest as
`ato.snapshot@1` would violate Capsule identity and Materializer semantics.

## Proposed boundary

Introduce a distinct restore-capable `ato.vm.snapshot@1` Materializer. It must
realize one already-existing `ComputationRef`; it must never derive logical
identity from VM bytes.

```text
ComputationRef C57
  ├─ ato.replay@1
  └─ ato.vm.snapshot@1
       ├─ exact VM/runtime format
       ├─ immutable runner compatibility contract
       ├─ content-addressed physical object graph
       └─ restore/activate/wait/quiesce Realization
```

The implementation belongs in a Materializer/runtime extension. Library,
Store, publisher, visibility, and legacy Snapshot IDs remain outside Ato Core.

## Required work before acceptance

1. Define a canonical descriptor that references the target `ComputationRef`,
   VM format, architecture, Firecracker version, runner class, restore
   contract, and all content-addressed physical objects.
2. Add a large-object transport that preserves complete Object closure without
   embedding hundreds of MiB in one `.capsule` HTTP upload.
3. Implement fail-closed host compatibility and restore through a
   `Realization` handle.
4. Prove quiesce-before-stop, process/TAP/vsock/profile cleanup, and no slot
   leak.
5. Build a new artifact from pinned Source when the old computation identity
   cannot be proven. Physical similarity is not equivalence.
6. Run bounded secret/private-data inspection before any public publication.

## Legacy inspection

`legacy-vm-inspector` is a read-only migration tool. It verifies the historical
JCS-canonical manifest digest, contiguous layer coverage, every unique chunk length, and
every unique BLAKE3 digest. It never restores, republishes, or prints chunk
contents, and its receipt explicitly states that integrity verification is not
a private-data scan.

Passing that inspection establishes only that the old physical graph is intact.
It does not establish a current Capsule identity, current compatibility, a
license right, or absence of private data.

## Non-goals

- restoring the old Store or launch API;
- making VM state a Core semantic primitive;
- using VM bytes as `ComputationRef`;
- silently treating a host mismatch as portable;
- embedding private state, credentials, ROMs, or user databases;
- app-specific Materializers or Adapters.
