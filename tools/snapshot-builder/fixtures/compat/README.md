# Snapshot v1 Compatibility Suite — fixtures

One directory per row of the fixture matrix in
[`docs/snapshot-v1-compatibility.md`](../../../../docs/snapshot-v1-compatibility.md) §4.
These directories ARE the contract's enforcement surface: a change that makes a
positive fixture stop sealing, or a negative fixture stop failing (or fail with
a different reason), is a contract break.

## Layout

Each fixture holds its app source + `capsule.toml` + `expected.json`
(machine-readable expectations). Two special shapes:

- `store-recipe-manifest-only/` ships **no** `capsule.toml`; its
  `store-recipe.toml` is what the seed script stores in
  `capsule_source_recipes.recipe_toml` — the claim carries it and the builder
  must treat it as authoritative (`manifest_source = "recipe_toml"`).
- `real-store-receipt-to-csv/` pins the external regression anchor
  (Koh0920/ato-receipt-to-csv) — expectations only, no app files.

## `expected.json` fields

| field | meaning |
|---|---|
| `class` | `positive` \| `negative` (contract table side) |
| `eligibility` | `pass` \| `fail` — the `derive_build_spec` verdict |
| `eligibility_reason_contains` | required substring of the rejection (when `fail`) |
| `runtime` / `probe_synthesized` / `start_cmd` | asserted spec fields (when `pass`) |
| `seal` | `sealed` \| `failed` — the end-to-end builder outcome |
| `seal_failure_stage` | builder ack `failure_stage` (`eligibility` / `build_ready_state` / `no_secret_scan`) |
| `advisory_pem_expected` | receipt must show the PEM advisory fired without gating |
| `manifest_source` | `repo` \| `recipe` \| `external` |

## How each layer consumes this

1. **KVM-free CI** — `crates/snapshot/tests/compat_fixtures.rs` runs the pure
   eligibility gate (`derive_build_spec` + `SourceProbe::scan`) over every
   fixture and asserts the `eligibility*` expectations, plus completeness
   (dirs ↔ contract table, both directions).
2. **Staging seed** — `ato-api scripts/staging/seed-snapshot-compat-recipes.ts`
   creates a Store capsule + approved recipe per fixture, pointing at
   `github://ato-run/ato@<commit>#tools/snapshot-builder/fixtures/compat/<name>`.
3. **API E2E** — enqueues every fixture and asserts the `seal*` expectations
   against the registry (`scripts/ready-state/`).
4. **Browser E2E** — drives the sealed fixtures through the PWA
   (`ato-pwa e2e/snapshot-compat.spec.ts`).

`planted-builder-token/` cannot carry a live credential in pinned public
source — see its README for the unit-level + fault-injection enforcement.
