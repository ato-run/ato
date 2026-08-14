# Provider Materialization

Status: Accepted

Providers own physical realization. They may consume computation constraints
and produce successor candidates or materialization evidence, but they do not
change semantic identity merely because the host mechanism differs.

`ato-provider-nacelle` realizes `ato.workspace@1`: runtime/tool lookup,
dependency installation, process spawn, filesystem preparation, secret
backend resolution, environment delivery, and OS sandbox enforcement. The
kernel does not depend on Nacelle.

Versioned runtime constraints resolve to the exact verified Nacelle artifact;
host `PATH` cannot override them. Every child starts from `env_clear` and gets
only provider-controlled `PATH`, `HOME`, locale baseline, declared environment,
and resolved secret values. Providers that cannot enforce an exact non-empty
network allowlist fail closed rather than widening it to unrestricted network.
Detached secret binding currently fails closed until a value-safe one-shot
transport is available.

`ato-provider-snapshot` registers opaque artifact refs plus an exact
host/provider realization contract and the unchanged `ComputationRef`.
Registration first resolves that computation from Objects. Verification checks
the contract, computation, and every artifact; it is not called restore because
no physical state is restored. Snapshot-owned reference extraction keeps
retained materialization artifacts in the generic Objects GC closure. VM state,
memory, filesystem images, or remote process handles are provider detail, not
Capsule identity. Capture rejects likely plaintext secret material.

`ato-netd` and `ato-tsnetd` own network enforcement and transport. Their DTOs
live in `ato-ipc`; semantic `ProtocolId` and Port definitions remain in
`ato-computation`.
