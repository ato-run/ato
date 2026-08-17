# Core architecture

Computation is the semantic center. The core libraries are:

- `ato-computation` — canonical semantic values, Ports, identity, and pure
  composition wiring;
- `ato-kernel` — Protocol-aware, payload-opaque evolution;
- `ato-compose` — validation and operational small-step composition;
- `ato-objects` — verified CAS, Records, lineage, closure traversal,
  signatures, and transport.

`ato-ipc` is an adjacent process wire, not a semantic protocol model.

Arrows point from a layer to what it depends on:

```text
apps / tools
    │ depends on
    ▼
adapters / materializers
    │ depends on
    ▼
compose       services
    │             │
    │ depends on  │ depends on
    ▼             ▼
kernel          ipc
    │
    │ depends on
    ▼
objects
    │
    │ depends on
    ▼
computation
```

## Boundaries

Protocol semantics owns logical interaction typing. Adapters connect physical
interaction to Protocols. Materializers physically encode or restore one
selected Computation point. Records observe transitions. None of these replaces
the Computation.

Distribution, placement, sandboxing, secrets, networking, process supervision,
containers, and VMs are realization concerns. They may constrain whether a
Capsule can run on a host, but do not define Capsule identity.

Run `cargo run -p arch-check`; it validates the dependency graph returned by
`cargo metadata`, including forbidden legacy package names.
