# runtime-image-ledger fixtures

These files are the reviewable examples for
[`docs/ops/runtime-image-ledger.md`](../../../docs/ops/runtime-image-ledger.md).
`valid-inventory.json` is a declaration set that passes every rule; each other
file breaks exactly one rule (or, for the last two, cannot be parsed).

Exit codes for every fixture: `valid-inventory.json` → 0, all others → 1.

| Fixture | Expected rule code | Field |
|---|---|---|
| `valid-inventory.json` (old affected image active + patched candidate verified) | — | — |
| `invalid-digest.json` | `image_digest_invalid` | `images[0].image_digest` |
| `duplicate-digest.json` | `image_digest_duplicate` | `images[1].image_digest` |
| `invalid-base-digest.json` | `base_digest_invalid` | `images[0].base_digests[0]` |
| `invalid-sbom-digest.json` | `sbom_digest_invalid` | `images[0].sbom.digest` |
| `runtime-version-range.json` | `runtime_version_not_exact` | `images[0].runtime_versions.node` |
| `invalid-scan-time.json` | `scan_checked_at_invalid` | `images[0].scan.checked_at` |
| `verified-without-receipt.json` | `receipt_required` | `images[0].receipt` |
| `receipt-not-fully-satisfied.json` | `receipt_not_fully_satisfied` | `images[0].receipt.fully_satisfied` |
| `receipt-derivation-mismatch.json` | `receipt_derivation_mismatch` | `images[0].receipt.derivation_ref` |
| `receipt-contract-mismatch.json` | `receipt_contract_mismatch` | `images[0].receipt.contract_ref` |
| `waiver-missing.json` | `waiver_required` | `images[0].findings_waiver` |
| `waiver-expired.json` | `waiver_expired` | `images[0].findings_waiver.expires_at` |
| `unknown-patch-reference.json` | `patch_reference_unknown` | `patch_candidates[0].from_digest` |
| `patch-self-reference.json` | `patch_self_reference` | `patch_candidates[0].to_digest` |
| `patch-contract-mismatch.json` | `patch_contract_mismatch` | `patch_candidates[0].rebuild.contract_ref` |
| `auto-build-not-satisfied.json` | `patch_auto_build_not_satisfied` | `patch_candidates[0].rebuild.fully_satisfied` |
| `advisories-empty.json` | `patch_advisories_empty` | `patch_candidates[0].advisories` |
| `schema-unsupported.json` | `schema_unsupported` | `schema` |
| `unknown-field.json` | `inventory_json_invalid` (deny_unknown_fields) | `document` |
| `malformed-json.json` | `inventory_json_invalid` | `document` |

The digests, contract refs, derivation refs, and receipt refs are synthetic but
well-formed (`sha256:` + 64 lowercase hex). They are not real images and were
not scanned, signed, or verified by anything.
