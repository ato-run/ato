# Formation, Verification and Receipt

Status: Current technical reference.

**Formation** takes explicit source/authoring input and produces a candidate
realization with a bound Contract K and Derivation D. Building, installing and
launching are Evolutions carried out through existing runtime mechanisms;
they do not define a second semantic lifecycle.

**Verification** compares observations with the unchanged K. Formation's seal
decision can allow `Satisfied` plus explicitly deferred runtime checks. A Run
is accepted only when all required observations are `Satisfied` and the
receipt reports `fully_satisfied = true`. A successful upload, a rendered PWA,
or one HTTP 200 is insufficient unless it satisfies the declared observations.

**Receipt** is evidence of that comparison. The shared Rust type
[`ContractVerificationReceipt`](../../lib/formation/src/verify.rs) owns the
`ato.contract-verification-receipt/1` schema. It names K, the selected D,
verification target, observations and overall result, with execution evidence
where supplied by that path. `bundle_sha256` is optional: Formation before
packaging has no bundle bytes to hash. Do not fabricate transport or attempt
evidence when a path has not supplied it.

CLI and Hosted execution consume the same verification authority. A receipt
must be associated with the Run/attempt being assessed, rather than a “latest
receipt” from another execution. A schema name by itself is not proof of
origin, full execution evidence, or every security property.

The implementation has separate boundaries:

| Boundary | Responsibility |
|---|---|
| Bundle validation | schema, digest, closure, profile and declared route consistency |
| Admission | selected D's runtime, platform, permissions, dependencies and limits |
| Execution | process/container and logical Port bindings |
| Verification | that Run's actual observations compared with frozen K |

The [Formation guide](../guides/formation.md) explains how to use these paths.
The [specification index](../specifications/README.md) identifies accepted
authority and implementation-track drafts separately.
