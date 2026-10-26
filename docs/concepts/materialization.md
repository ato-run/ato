# Materialization

Continuing the [compiler-error example](computation.md): Alice sealed `C42`
into a Capsule and sent it to Bob. Two different questions are involved:

```text
Capsule            answers: where should I continue from?
Materialization     answers: how do I get back there, on this machine?
```

A **Materialization** is a physical way to realize one logical Capsule. When
Bob runs `ato run build-error.capsule`, Replay is what actually reconstructs
`C42` on his machine — recreating the workspace files and restarting the
build's PTY session by replaying the recorded interactions that produced
`C42` in the first place.

```text
logical continuation
        !=
physical realization
```

Possible strategies (see [current implementation](#current-implementation)
for which of these actually exist today):

```text
Capsule C42
├─ Replay                     (implemented, restore-capable)
├─ filesystem reconstruction
├─ source reconstruction
├─ process checkpoint         (model/future)
├─ container checkpoint       (model/future)
├─ VM snapshot                (backend-specific implementation)
└─ remote live state          (model/future)
```

The Capsule identity and Materialization identity are separate. Adding,
replacing, or removing a compatible Materialization does not create a new
Capsule — Replay reconstructing `C42` and a compatible VM-snapshot
restore of `C42` would both be *the same Capsule*, realized two different
ways. Conversely, two artifacts with similar bytes do not prove that they
realize the same Capsule.

A logical Capsule can exist before a compatible physical Materialization is
available on a particular host. Materialization availability determines
whether and how that host can resume it, not what the Capsule is.

## Binding and runtime endpoint

A restored Computation still needs runtime resources: Bob's shell Port needs
an actual PTY, the workspace Port needs an actual directory on Bob's disk. A
**Binding** maps a logical requirement — currently a Port, identified by
`PortId` — to a physical resource such as a PTY, socket, or file descriptor.
This document calls that physical resource a **runtime endpoint** (lowercase)
to avoid colliding with the core library's `Endpoint` type, which is an
unrelated composition-graph wiring detail — see
[Port and Endpoint](computation.md#port-and-endpoint-theory-vs-implementation).
Runtime-endpoint details are realization-specific and do not enter the
logical Port identity. A persistent, realization-independent reference to a
Port (sometimes called a `PortRef` in the theory) is model/future work, not a
type in the current codebase.

## Current implementation

- Replay is restore-capable through live Adapters and verified Record chains.
- Workspace snapshot materializers capture and verify artifacts; their restore
  capability remains verify-only.
- The generic VM snapshot materializer reports restore support through an
  injected backend and RunnerCapabilities. This does not prove arbitrary host,
  process or distributed capture support.
- Portable Instance snapshots use a separate bounded saved-data profile.

See [Snapshot materialization](../snapshot.md) for code references and current
limits. The accepted [Materialization RFC](../rfcs/accepted/MATERIALIZATION.md)
remains the computation-root authority; implementation-track VM/portable
contracts are still drafts.
