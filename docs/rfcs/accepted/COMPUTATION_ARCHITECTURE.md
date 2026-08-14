# Computation Architecture

Status: Accepted

## Identity

The only canonical semantic value is:

```text
ComputationObject { semantics, boundary, residual }
```

Canonical JCS encoding and BLAKE3 derive `ComputationRef`. A Capsule is one
selected immutable computation point:

```text
Capsule = ComputationRef
Run     = mutable cursor { head: ComputationRef }
```

History, branch names, Records, timestamps, host paths, bindings, active
processes, and Materializations never enter `ComputationObject`.

For `C0 → C1 → C2`, the current computation is C2. History may relate all
three refs, but C2 does not hash its history.

## Evolution

The Kernel resolves `Run.head`, dispatches by `SemanticsId`, validates opaque
Protocol payloads through `ProtocolSemantics`, seals the successor, and moves
the cursor. `derive_transition` does not publish evidence;
`commit_transition` publishes one selected transition; `step` combines both
and advances `Run.head`.

Kernel code never learns HTTP, PTY, workspace, binding, runtime, snapshot, or
process payload schemas. Concrete Adapters remain outside Kernel dependencies.

## Ports and Protocols

A Port is an interaction crossing the selected computation boundary. It has a
stable `PortId`, `ProtocolId`, and `RoleId`. `ProtocolPayload` is opaque to the
Kernel and typed by registered Protocol semantics.

Recorded, replayable, and runnable are separate properties. An Adapter may
observe without applying, or verify without restoring.

## Composition

Composition is a basic Ato operation:

```text
composeW(C1, …, Cn) = C
```

Pure wiring types (`NodeId`, `Endpoint`, `Connection`, and
`CompositeResidual`) live in `ato-computation`. Executable validation,
synchronization, hiding, small-step reduction, and closure traversal live in
the core `ato-compose` library because they require Kernel and Objects.

Child-to-child communication is internalized as `Tau`; exported child Ports
form exactly the parent boundary. Kernel still has no workload-specific graph,
service, placement, or cluster primitive.

## Classification

Every new public concept must be one of:

1. a property of the current Computation;
2. history or evidence in Objects;
3. a Protocol interaction;
4. a physical Adapter;
5. a Materialization implementation; or
6. CLI/runtime orchestration.

No additional semantic root is introduced for lineage, runtime, provider,
repository, snapshot, or application category.
