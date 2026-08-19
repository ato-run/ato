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
read input/output at the gate and drains work that already crossed it. Binding
has no value serialization and Workspace reconciliation is Supervisor-owned
while every interaction frontier is held.

v0 current capture is explicitly limited to an **adapter-mediated frontier**.
A Process may opt into `capture = "adapter_mediated"` only when all semantic
Evolution crosses the attached HTTP/PTY/Binding/Workspace boundaries. The
Process barrier itself does not freeze timers, background threads, or arbitrary
filesystem mutation. A Process without that declaration reports
`capture_consistency = unsupported`, and `encap --current` fails closed. A
future runtime-frozen policy requires a real process freeze plus an atomic
state projection; it must not be inferred from the presence of a process.

Portable sessions import into a temporary local computation repository and
create a private branch rooted at the immutable bundle root. New interactions
advance only that temporary branch. Re-encapsulation exports its current point
without mutating the source bundle or its parent Capsule.

The canonical machine verifier remains Rust `ato-objects`. Service validation
may invoke a hidden machine-facing CLI, but may not independently reinterpret
the bundle security format in TypeScript.

## Hosted Terminal Surface boundary

A hosted `ato.pty@1` Port is projected through `ato.terminal.v1`; it is not a
raw TCP forwarding target. The public Host gateway validates the exact Origin,
the short-lived Surface Assertion, its `session_id`/`surface_id` scope, one-time
`jti`, and the WebSocket subprotocol before it opens the sandbox-local Unix
socket. Assertion keys and allowed-origin policy remain in the Host process and
must not enter the sandbox environment, Capsule bundle, Record payload, page
realm, receipt, or logs.

The sandbox PTY Adapter owns only the terminal protocol endpoint. It negotiates
`ato.terminal.v1`, emits a bounded `ready` control before terminal bytes, treats
binary frames as PTY input, and validates typed `resize`/`ack` text controls.
Text control frames are never shell input. This preserves the Adapter's
observe/apply/ack/quiesce responsibility while the Host gateway owns product
access policy and replay protection.

Both ordinary Continue Runs and view-only Hosted Replay use the same gateway.
An absent, expired, forged, wrong-scope, or already-consumed assertion fails
closed before any sandbox connection. A direct request to the runner origin is
therefore not an access capability.
