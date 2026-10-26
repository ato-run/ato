# Execution identity and diagnostics

Computation-root identity is the BLAKE3 `ComputationRef` derived from canonical
`ComputationObject` bytes. Portable application identity is the SHA-256
`ContractRef` derived from canonical `BoundContract` bytes. Its selected
`DerivationRef` names the declared realization route; the bundle SHA-256 names
transport bytes. These values are distinct.

An actual execution additionally has Run/attempt, lease and realization
evidence. A PID, container ID, host path or current endpoint is not logical
Capsule identity. No generic `ExecutionIdentity` aggregate is assumed here.
Use the actual attempt's receipt and recorded execution evidence when
diagnosing drift; do not substitute a later successful receipt or deployment
version for that evidence.

See [profiles and identity](concepts/application-contracts.md),
[receipt boundaries](concepts/formation-verification.md) and
[state continuity](concepts/state-continuity.md).

<!-- Compatibility anchors from the previous documentation entry. -->
<a id="computation-identity-and-diagnostics"></a>
