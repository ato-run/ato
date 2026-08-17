# Snapshot materialization

A snapshot is a physical Materialization. It is not a Capsule, does not
replace a `ComputationRef`, and does not make VM or process state part of the
Semantic Core.

The current `ato.snapshot@1` implementation is specifically a
workspace/filesystem Materialization. It records physical artifacts and an
exact host compatibility contract, verifies the target Computation and every
artifact, and keeps referenced content live through object-store traversal.
Likely plaintext secrets are rejected.

Its current restore capability is **verify-only**. There is no public
`ato snapshot` command and it must not be presented as a process or VM
checkpoint backend.

See [Materialization](concepts/materialization.md) and the accepted
[Materialization RFC](rfcs/accepted/MATERIALIZATION.md).
