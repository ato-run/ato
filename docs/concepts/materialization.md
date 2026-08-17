# Materialization

A **Materialization** is a physical way to realize one logical Capsule.

```text
logical continuation
        !=
physical realization
```

Possible strategies include:

```text
Capsule κ
├─ Replay
├─ filesystem reconstruction
├─ source reconstruction
├─ process checkpoint
├─ container checkpoint
├─ VM snapshot
└─ remote live state
```

The Capsule identity and Materialization identity are separate. Adding,
replacing, or removing a compatible Materialization does not create a new
Capsule. Conversely, two artifacts with similar bytes do not prove that they
realize the same Capsule.

A logical Capsule can exist before a compatible physical Materialization is
available on a particular host. Materialization availability determines
whether and how that host can resume it, not what the Capsule is.

## Binding and Endpoint

A restored computation still needs runtime resources. A **Binding** maps a
logical requirement, such as a PortRef, to a physical **Endpoint** or other
provider resource. Endpoint details are realization-specific and do not enter
the logical Port identity.

## Current implementation

- `ato.replay@1` is restore-capable and reconstructs a target by applying the
  required Record chain through live Adapters.
- `ato.snapshot@1` captures and verifies a workspace/filesystem artifact with
  an exact host compatibility contract. It is currently verify-only.
- General process checkpoints, VM snapshots, cross-host resume, and automatic
  equivalence between heterogeneous Materializations are model/future work.

See the accepted [Materialization RFC](../rfcs/accepted/MATERIALIZATION.md).
