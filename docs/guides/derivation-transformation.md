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

## Recorded experiments

The [v0 experiment](../archive/2026/ops/contract-preserving-transformation-v0.md)
and the [stateful scenario experiment](../archive/2026/ops/stateful-transformation-v1.md)
recorded this path on Linux CI. The table below summarizes what each record
claims; the archived pages, written partly in Japanese, remain the evidence.
**None of it was re-executed for this page.** These are historical
measurements, not a current verification result, a deployment state or a
production availability claim.

| Field | v0 contract-preserving transformation | v1 stateful transformation |
|---|---|---|
| Claim | Two Derivations built from different source closures satisfy the same frozen K: `ContractRef` and the canonical Contract bytes stay identical, `DerivationRef` and the source closure differ, the new receipt is `fully_satisfied: true`, and the baseline bundle and receipt stay byte-identical. | A candidate with different source **and** a different internal storage schema satisfies the same frozen K (HTTP root plus `ato.scenario/1`), and the canonical scenario transcript digest matches the baseline. |
| Baseline | `ato pack` / `ato run` / `ato reform` (PR #1557); evidence record committed 2026-10-25. | `ato pack` / `ato run` / `ato reform` (PR #1557, #1559) plus Scenario Verifier v1 (#1560, #1561); evidence record committed 2026-10-25. |
| Environment | Linux with bubblewrap and unprivileged user namespaces permitted by AppArmor; Python 3.12.7 at `/opt/ato/toolchains/python/3.12.7`; repository root as the working directory. | The same Linux prerequisites (bubblewrap, unprivileged user namespaces, Python 3.12.7 at the same path), matching CI. |
| Command | `cargo test -p ato-cli --test derivation_transformation --locked -- --nocapture` | `cargo test -p ato-cli --test stateful_transformation --locked -- --nocapture` |
| Artifacts | `evidence.json` (schema `ato.derivation-transformation-evidence/1`) under `target/transformation-v0-evidence/`, with `report.json`, `receipt.json` and `candidate.capsule`; CI uploads the directory as `transformation-v0-evidence`. | `evidence.json` (schema `ato.stateful-transformation-evidence/1`) under `target/stateful-transformation-evidence/`, with `report.json`, `receipt.json`, `candidate.capsule` and `scenario-transcripts/crud.json`; CI uploads the directory as `stateful-transformation-evidence`. |
| Report / receipt fields | `report.json` carries `frozen_contract_ref`, `old_source_closure_refs`, `new_source_closure_ref`, `status` and `k_preserved`; `receipt.json` carries `contract_ref`, `derivation_ref` and `fully_satisfied`. | `evidence.json` carries the baseline and candidate `contract_ref`, `derivation_ref` and source closure refs, the scenario `ref` and `transcript_digest`, and the record's `non_claims`; the receipt observation uses `path = "scenario:crud"` with `digest` and `body_sha256`. |
| Negative cases | Behavior break rejected (`k_preserved: true`, `fully_satisfied: false`); contract change refused before build (`candidate_contract_mismatch`, no `candidate.capsule` or `receipt.json`); source-bound K refused (`frozen_contract_is_source_bound`); a candidate manifest without `[contract]` is built and accepted. | Behavior break rejected (`k_preserved: true`, `fully_satisfied: false`, candidate written with a failed receipt); K redefinition refused before build (`candidate_contract_mismatch`, nothing written); baseline bundle and receipt SHA-256 unchanged after every case. |
| Non-claims | No publish, activate, switch or rollout; no Hosted or ato-api E2E; no scenario verifier; equal K is not equal functionality; a frozen v4 bundle carrying an Instance snapshot is refused (`frozen_instance_snapshot_unsupported`) and v0 candidate transport is v3; Linux local Runtime only, macOS and Windows execution unverified. | No persistence across a process restart — the scenario is fresh-instance only; scenario coverage is representative CRUD, authorization and input-error paths, not exhaustive; equal K is not equal functionality or security; no Hosted, ato-api or staging execution, Linux local Runtime CLI E2E only; no portability or generality claim for the fixtures. |

Both records therefore cover a fresh CLI-local Run only. Neither proves
restart persistence, Hosted execution or a deployment state, and neither was
re-measured here. Read [compatibility](../concepts/compatibility.md) before
using the result to justify an upgrade.
