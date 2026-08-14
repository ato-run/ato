# Materialization

Status: Accepted

A Materializer physically encodes or restores one exact `ComputationRef`. It
is distinct from an Adapter, which handles external interaction.

`ato-materializer-api` defines one registry path for built-in and third-party
implementations. Every Materializer reports restore capability, encodes,
verifies, classifies compatibility, and may restore.

Replay is restore-capable and protocol-generic. Each Record dispatches by
`adapter_id` through `AdapterRegistry`; generic Replay has no Protocol switch.
Encoding or restoration fails when a required Adapter lacks `apply`.

Snapshot registers and verifies artifact refs plus an exact host compatibility
contract. It is verify-only until a physical restore exists. Snapshot evidence
never changes the target Computation identity.
