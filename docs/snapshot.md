# Snapshot materialization

Snapshots are provider-owned physical materializations. They are not Capsule
identity and do not replace a `ComputationRef`.

`ato-provider-snapshot` registers opaque artifacts in Objects with an exact
provider/host realization contract. Registration verifies that the referenced
Computation resolves. Verification checks the contract, computation, and every
artifact, then returns the unchanged computation reference; it does not claim
to restore physical state. Retained metadata keeps its artifact refs live
through Objects GC. Registration rejects likely plaintext secrets.

```bash
ato snapshot capture COMPUTATION_REF vmstate.bin memory.bin
ato snapshot verify MATERIALIZATION_REF
```
