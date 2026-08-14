# Protocol Adapter

Status: Accepted

`ato-adapter-api` defines the only registration path for built-in and
third-party Adapters. Each Adapter declares observe, apply, verify, and
quiesce capabilities and implements preflight, attach, observation, optional
application/verification, quiesce, and detach operations.

Protocol semantics defines the logical type, role, and behavior of Port
interaction. An Adapter connects real interaction to that Protocol. They are
not interchangeable, and Kernel never decodes Adapter payload schemas.

Built-in v1 Adapters are Process, PTY, Workspace, Binding, and HTTP. PTY records
bytes, resize, signal, and attachment events without inferring shell commands.
HTTP request and response are distinct Records. Binding evidence contains only
logical and safe provider-reference identity; secret values remain runtime
inputs and are never persisted.
