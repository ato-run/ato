# Snapshot materialization

Snapshots are provider-owned physical materializations. They are not Capsule
identity and do not replace a `ComputationRef`.

`ato-provider-snapshot` captures opaque artifacts into Objects with an exact
provider/host realization contract. Restore verifies the contract and every
artifact, then returns the unchanged computation reference. Capture rejects
likely plaintext secrets.

```bash
ato snapshot capture COMPUTATION_REF vmstate.bin memory.bin
ato snapshot restore MATERIALIZATION_REF
```
