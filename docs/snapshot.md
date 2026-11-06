# Snapshot materialization

Status: Current technical reference.

A snapshot is a physical realization artifact. Its digest, backend and host
compatibility evidence are distinct from logical Capsule identity.

| Path | Current code / limitation |
|---|---|
| workspace snapshot | `ato.materialize.workspace.snapshot@1`; legacy `ato.snapshot@1` reader; captures/verifies filesystem artifacts and remains verify-only |
| VM snapshot | `ato.materialize.vm.snapshot@1`; restore-capable interface with an injected backend and RunnerCapabilities; no universal host/process checkpoint promise |
| portable Instance snapshot | wire v4 saved-state closure; bounded declared resources/assets and Contract requirements; separate from VM memory |

The workspace implementation is in
[snapshot materializer](../extensions/materializers/snapshot/src/lib.rs).
The [VM materializer](../extensions/materializers/vm-snapshot/src/lib.rs)
reports `RestoreCapability::Supported`, but actual restoration requires its
backend and a compatible host. The [VM draft](rfcs/draft/VM_SNAPSHOT_MATERIALIZATION.md)
and [Instance draft](rfcs/draft/PORTABLE_INSTANCE_SNAPSHOT.md) remain
implementation-track contracts. The accepted
[Materialization RFC](rfcs/accepted/MATERIALIZATION.md) is unchanged.

There is no general public `ato snapshot` command. A tested backend or a
fresh-instance scenario does not establish arbitrary cross-host resume,
distributed capture or state migration. Read
[State Continuity](concepts/state-continuity.md) before making a persistence
claim. Former Ready-State experiments remain in the
[historical subtree](ready-state/README.md).
