# Verify a Derivation transformation

Status: Current technical reference.

`ato reform` builds and verifies a local candidate under an existing portable
bundle's frozen K. Review the candidate source before executing it. Start with
an existing bundle whose Contract permits the intended source change.

```sh
target/debug/ato reform ./existing.capsule ./candidate-source \
  --out .tmp/reform-candidate
```

The output directory must be new. Supply `--derivation <declared-ref>` if the
candidate has multiple Ds. The candidate manifest may omit `[contract]`; when
it includes one, its canonical bytes must equal frozen K before building.

Review `report.json`, `receipt.json` and `candidate.capsule` together. An
accepted candidate must preserve K and satisfy its observations. Existing
bundle/receipt bytes remain intact. A contract redefinition is refused before
execution; a behavioral failure has a failed verification result.

Source-bound K is refused for source replacement. The current transformation
path also refuses a frozen bundle with an Instance snapshot; do not infer a
saved-data upgrade from a fresh-instance success. It does not publish,
activate, switch, or automatically roll out the candidate.

The [v0 experiment](../archive/2026/ops/contract-preserving-transformation-v0.md)
and [stateful scenario experiment](../archive/2026/ops/stateful-transformation-v1.md)
record exact fixtures, negative cases and Linux prerequisites. They are
historical evidence, including Japanese source text, rather than production
availability claims. Read [compatibility](../concepts/compatibility.md) before
using the result to justify an upgrade.
