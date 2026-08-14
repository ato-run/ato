# Capsule Network Runtime

Status: Draft

The network product projects existing Capsule objects into transport, Post,
Run, and fork resources. It introduces no new semantic root. A Capsule remains
one immutable `ComputationRef`; a `.capsule` remains a transport bundle; a live
Run remains a mutable cursor outside computation identity.

`ato encap <selector> --current` exports the active Run frontier without
advancing the sealed branch ref or terminating the Run. It is the only local
current-point portability boundary; there is no `capture` lifecycle command.

Every live Adapter that can introduce Evolution must advertise and implement a
non-destructive capture barrier. The Supervisor pauses admission, drains
already-admitted observations, reconciles the workspace through the active Run
transaction, publishes the exact computation and Record frontier under a
capture token, and remains paused while the CLI assembles the bundle. The CLI
releases the token on success and failure. An expired lease is also released so
a dead CLI cannot permanently pause a durable Run. Adapters without a safe
barrier make current capture fail closed.

HTTP closes request admission and drains the active exchange. PTY holds newly
read input/output at the gate and drains work that already crossed it. Process,
Binding, and Workspace barriers are non-destructive no-ops; workspace
reconciliation is Supervisor-owned while every interaction frontier is held.

Portable sessions import into a temporary local computation repository and
create a private branch rooted at the immutable bundle root. New interactions
advance only that temporary branch. Re-encapsulation exports its current point
without mutating the source bundle or its parent Capsule.

The canonical machine verifier remains Rust `ato-objects`. Service validation
may invoke a hidden machine-facing CLI, but may not independently reinterpret
the bundle security format in TypeScript.
