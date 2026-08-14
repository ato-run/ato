# Materialization

Status: Accepted

A Materializer physically encodes or restores one exact `ComputationRef`. It
is distinct from an Adapter, which handles external interaction.

`materialize` is an operation performed by a Computation playing the
Materializer role. A Materialization artifact is a Value produced by that
operation; it is not itself a Computation or a second Capsule identity. `encap`
is likewise an operation that may invoke one or more Materializer computations
and transport their artifact Values beside the unchanged logical root.

`ato-materializer-api` defines one registry path for built-in and third-party
implementations. Every Materializer reports restore capability, encodes,
verifies, classifies compatibility, and may restore.

Restore returns a physical `Realization` handle, not a claimed
`ComputationRef`. A restore-capable Materializer owns reconstruction from its
declared anchor through the target and hands the resulting runnable resources
to the caller. The caller must not restore the target workspace or spawn the
target runtime before choosing the Materializer.

A Realization exposes activate, wait, and quiesce lifecycle operations. Both an
ephemeral portable run and a durable local resume consume this same verified
handle; orchestration policy does not bypass reconstruction.

Replay is restore-capable and protocol-generic. It starts a realization at the
descriptor anchor, checks every `head_before` against the causally derived
head, applies the Record through the matching live Adapter instance, and only
finishes when the derived head equals the descriptor target. Generic Replay
has no Protocol switch. Encoding or restoration fails when a required Adapter
lacks `apply`.

The current `ato.snapshot@1` is specifically a workspace/filesystem snapshot,
not a process or VM checkpoint. It encodes at least one physical artifact,
registers and verifies its refs plus an exact host compatibility contract, and
is verify-only until a physical restore exists. A metadata-only descriptor is
not a Snapshot.
Snapshot evidence never changes the target Computation identity.
