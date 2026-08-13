# Provider Materialization

Status: Accepted

Providers own physical realization. They may consume computation constraints
and produce successor candidates or materialization evidence, but they do not
change semantic identity merely because the host mechanism differs.

`ato-provider-nacelle` realizes `ato.workspace@1`: runtime/tool lookup,
dependency installation, process spawn, filesystem preparation, secret
backend resolution, environment delivery, and OS sandbox enforcement. The
kernel does not depend on Nacelle.

`ato-provider-snapshot` stores opaque artifact refs plus an exact host/provider
realization contract and the unchanged `ComputationRef`. Restore verifies the
contract and every artifact and returns that computation ref. VM state,
memory, filesystem images, or remote process handles are provider detail, not
Capsule identity. Capture rejects likely plaintext secret material.

`ato-netd` and `ato-tsnetd` own network enforcement and transport. Their DTOs
live in `ato-ipc`; semantic `ProtocolId` and Port definitions remain in
`ato-computation`.
