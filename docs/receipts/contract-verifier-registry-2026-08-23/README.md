# Contract verifier registry receipt — 2026-08-23

Status: implementation complete; Draft PR open

## Stack

- repository: `ato-run/ato`
- base: `feat/async-record-writer-frontier-v1`
- verified base head: `49fdc82a09a857e58324ea921942e43da4398745`
- dependency: Draft PR `ato-run/ato#1293`
- branch: `feat/contract-verifier-registry-v1`
- Draft PR: `https://github.com/ato-run/ato/pull/1294`

## Implemented boundary

- extension-defined `ContractDescriptor` and `ContractVerifierRegistry`;
- replay/restore completion separated from Contract acceptance;
- hidden candidate activation separated from external Surface publication;
- deterministic verifier lookup with fail-closed missing-verifier behavior;
- cleanup on activation, Contract, publication, wait, and accepted-candidate drop paths;
- canonical Contract carriage in `ato.replay@2` only;
- legacy `ato.replay@1` wire compatibility retained;
- HTTP Surface binding delayed until `publish`;
- loopback HTTP and bounded workspace digest verifier extensions.

## Identity

Contract descriptors and results do not generate or update ComputationRef.
They assert whether a candidate can be accepted as a Realization of an
already-known target.

## Verification

- materializer API Contract lifecycle tests: PASS (3/3 Contract-specific)
- builtin Contract extension tests: PASS
- HTTP hidden-Surface lifecycle test: PASS
- replay v2 Contract roundtrip: PASS
- computation architecture: PASS (13/13)
- `cargo fmt --all -- --check`: PASS
- `cargo check --workspace --all-targets --locked`: PASS
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`: PASS
- `cargo run -p arch-check --locked`: PASS (23 workspace packages)
- full workspace: all targets PASS except the two known macOS netd tests in the long worktree path (`SUN_LEN`)
- the same two netd tests at this exact head in the short `.tmp/a3s` worktree: PASS (2/2)

## Production

- inserted rows: 0
- updated rows: 0
- deleted rows: 0
- uploaded production objects: 0
- production approval requested: false
