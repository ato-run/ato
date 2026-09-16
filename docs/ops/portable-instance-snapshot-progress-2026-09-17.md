# Portable Instance snapshot progress — 2026-09-17

## Scope

Repositories and branches:

- `ato-run/ato` — `feat/portable-instance-snapshot`, stacked on
  `feat/portable-durable-local-instance` at `1a07ea20`
- `ato-run/ato-api` — `feat/portable-instance-snapshot`, stacked on
  `feat/portable-v4-hosted-transport` at `31f12a0f`
- `ato-run/ato-pwa` — `feat/portable-instance-snapshot`, stacked on
  `feat/portable-v4-hosted-transport` at `3f3cf89`

Implementation commits:

- `c55b1dea` — v4 snapshot object, Contract binding, closure validation, and
  exact runtime observation
- `123c4949` — durable local materialization, independent Asset IDs, snapshot
  save, and `ato app export`
- `05078db1` — semantic snapshot-content validation and an authenticated
  validator report/upload lane for Hosted restore
- `da33b23b` (`ato-api`) — restore browser state, JSON Data, and Assets into a
  fresh Instance before launch and persist exact restore evidence
- `4a47865` (`ato-pwa`) — require and display saved-data restore evidence

Draft pull requests:

- `ato-run/ato#1357`
- `ato-run/ato-api#652`
- `ato-run/ato-pwa#394`

No staging or production deployment was performed.

## Implemented

- Added optional v4 `index.instance_snapshot_ref`; v3 rejects the extension.
- Added `ato.portable-instance-snapshot/1` with JSON/browser-state resources
  and immutable Assets.
- Added `ato.contract.instance-snapshot@1` and receipt evidence.
- Snapshot data changes mint a new ContractRef while keeping DerivationRefs.
- Snapshot replacement prunes only objects no longer reachable from any root.
- thin/cached/offline repack preserves K, D, and the snapshot ref.
- Local import verifies and materializes snapshot bytes under the new Instance.
- Portable Asset aliases receive deterministic Instance-local `ast_...` IDs;
  two independent imports do not reuse IDs.
- `save_snapshot` is fenced while a Run is active, writes immutable bundle and
  snapshot data first, then atomically replaces Instance metadata.
- `ato app export <instance-id>` exports the current immutable bundle.
- Snapshot resource bytes are semantically validated by Rust: JSON is
  canonical, browser-state keys are sorted and unique, protocols and limits
  are bounded, and Asset metadata is safe.
- The validator uploads snapshot content through a bundle-scoped lane separate
  from public static content and reports the authenticated descriptor.
- Hosted import restores browser state, JSON Data Resources, and Asset bodies
  into a fresh Instance before launch. Receiving Resource and Asset IDs are
  recorded in a bounded alias map; origin IDs are not accepted or reused.
- A failed restore deletes the fresh Instance. A publication race after launch
  also requests that specific lease to stop before the Instance is deleted.
- Runtime verification receives the persisted restored snapshot digest. The
  PWA requires a satisfied `instance-snapshot` observation before Ready and
  displays `Saved data — Restored` separately from Contract and Surface state.

## Verification

Passed:

```text
cargo test -p ato-objects -p ato-formation -p ato-portable-application \
  -p ato-cli --lib --tests

ato-cli unit                         27 passed
ato-cli portable integration        10 passed
ato-formation unit                  64 passed
ato-formation integration           66 passed
ato-objects                         30 passed
ato-portable-application            37 passed
```

Passed:

```text
cargo clippy -p ato-objects -p ato-formation \
  -p ato-portable-application --all-targets -- -D warnings

cargo clippy -p ato-cli -p ato-formation-worker \
  --all-targets --no-deps -- -D warnings
```

The combined dependency-inclusive CLI clippy command still stops on the
unchanged parent warning at `lib/sandbox/src/macos.rs:213`
(`clippy::redundant_closure`). The warning is present before these commits and
was not suppressed.

API verification passed:

```text
pnpm typecheck
portable snapshot/schema/Asset tests        23 passed
focused validator-authenticated report test  1 passed
```

The full Capsule Network registry test has three unrelated route failures.
The same three failures reproduce unchanged on the exact parent commit
`31f12a0f`. The migration-chain test also fails on both branches before test
collection because workerd cannot import `node:os` from
`@better-auth/telemetry`. `pnpm lint` cannot start because this repository does
not install an `eslint` executable; no lint failure was hidden or suppressed.

PWA verification passed:

```text
npm run typecheck
focused API client and Ready-page tests       5 passed
npm run build
```

## Negative coverage

- v3 snapshot extension is rejected.
- Missing snapshot content is rejected.
- Content with the wrong digest is rejected.
- Materialized Asset tamper is rejected before use.
- Snapshot K without an exact runtime restore observation fails with
  `instance_snapshot_missing`.
- v4 validator reports may contain snapshot descriptors; v3 reports reject
  that field.
- Hosted finalization rejects a missing, wrong-sized, or unauthenticated
  snapshot blob before import.
- Hosted runtime verification receives the snapshot digest only after restore
  evidence was persisted for that import.
- An active Run cannot seal a new snapshot.
- Replacing saved data changes K, preserves D, and removes the old unreachable
  snapshot/resource objects.
- CLI integration imports one snapshot bundle twice, observes distinct local
  Asset IDs with equal body digests, and re-exports the original bytes.
- CLI integration confirms a snapshot-bearing Run fails until an Adapter emits
  exact restore evidence.

## Not complete

This is the object/K/local-storage boundary, not the complete saved-data
roundtrip acceptance.

- No browser-state flush/capture from a live Hosted Instance is connected.
- No PWA export UI is connected; PWA import and restored-data status are
  connected.
- No field-aware `ato-asset://` alias resolution is connected.
- JSON Data Resources and Assets are restored and rebound, but Applications do
  not yet have a general declared Port/binding that consumes those aliases.
- No filesystem state snapshot is included in this increment.
- No staging deployment or browser roundtrip acceptance was performed.
