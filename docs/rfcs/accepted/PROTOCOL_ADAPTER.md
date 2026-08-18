# Protocol Adapter

Status: Accepted

`ato-adapter-api` defines the only registration path for built-in and
third-party Adapters. An `AdapterFactory` validates an `AdapterInstance` and
attaches one stateful `AttachedAdapter`. The attached instance, rather than a
new registry singleton, owns observation, application/verification, quiesce,
detach, activation, and waiting for its live resources.

The Supervisor may not switch on Adapter IDs. It constructs every configured
instance through the registry, completes every attach before publishing
`ACTIVE`, and asks those same live instances to quiesce before terminating the
owned process group. Observation persistence errors are part of quiesce and
must not be discarded.

Protocol semantics defines the logical type, role, and behavior of Port
interaction. An Adapter connects real interaction to that Protocol. They are
not interchangeable, and Kernel never decodes Adapter payload schemas.

Workspace capture is an Adapter boundary with explicit rooted-relative include
and exclude paths. The default policy prevents common credential-bearing files
such as local environment files, credential stores, private keys, repository
metadata, and `.capsule/` from being captured, but is not a proof that arbitrary
secret-like filenames are absent. These exclusions always win over an include.
Every workspace/filesystem Materialization uses this same policy.

Built-in v1 Adapters are Process, PTY, Workspace, Binding, HTTP, and Browser. PTY records
bytes, resize, signal, and attachment events without inferring shell commands.
HTTP request and response are distinct Records. Binding evidence contains only
logical and safe provider-reference identity; secret values remain runtime
inputs and are never persisted.

HTTP v1 conservatively classifies every inbound request as Evolution and every
outbound response as evidence. HTTP safe-method vocabulary does not imply a
semantic no-op. A later ProtocolSemantics implementation may derive a more
precise effect; the physical Adapter must not infer application purity.

Process execution starts from an empty environment. Only the minimal explicit
platform base environment and declared Binding projections may cross into the
computation.

Browser v1 records top-level physical keyboard control/navigation, pointer,
click, and scroll input as inbound Evolution. Its generic Bridge validates an
exact origin plus runtime-only channel and browser-session credentials. Replay
applies one canonical Browser event at a time through the ordinary
`AttachedAdapter` path and waits for Bridge acknowledgement. Runtime discovery
and credentials are never Computation, Record, authoring, or bundle data.
