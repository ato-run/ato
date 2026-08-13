# Computation identity and diagnostics

Canonical `ComputationObject` bytes derive the BLAKE3 `ComputationRef`.
Identity includes semantics, boundary, and current residual future—not
transition history or provider host facts.

Drift is diagnosed by comparing computation refs and adapter/provider receipts.
Repository resolution evidence explains source/toolchain choices; provider
receipts explain physical realization. These derived diagnostics do not form a
generic ExecutionIdentity aggregate.
