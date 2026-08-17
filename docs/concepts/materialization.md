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
├─ VM snapshot                (model/future)
└─ remote live state          (model/future)
```

The Capsule identity and Materialization identity are separate. Adding,
replacing, or removing a compatible Materialization does not create a new
Capsule — Replay reconstructing `C42` and a hypothetical future VM-snapshot
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

- `ato.replay@1` is restore-capable and reconstructs a target — such as
  `C42` — by applying the required Record chain through live Adapters. This
  is what powers `ato run build-error.capsule` in the README example today.
- `ato.snapshot@1` captures and verifies a workspace/filesystem artifact with
  an exact host compatibility contract. It is currently verify-only — it
  cannot yet restore a point on its own.
- General process checkpoints, VM snapshots, cross-host resume, and automatic
  equivalence between heterogeneous Materializations are model/future work.

See the accepted [Materialization RFC](../rfcs/accepted/MATERIALIZATION.md).
