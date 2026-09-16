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

The first implementation was the object/K/local-storage boundary rather than
complete saved-data roundtrip acceptance. The stacked Hosted export checkpoint
below closes its browser flush, PWA export, and field-aware Asset-alias gaps.
Filesystem state, general declared bindings, staging deployment, and browser
roundtrip acceptance remain incomplete.

## Hosted export implementation checkpoint

The stacked `feat/portable-hosted-export` increment adds the missing live
Hosted capture and `.capsule` download path. Implementation commits:

- `c5743dff` (`ato`) — Rust-authoritative Hosted repack/snapshot builder,
  dependency hydration shared with the CLI, exact Asset alias bindings, and
  validator-agent export jobs;
- `d0077e7f` (`ato-api`) — migration 0272, capture/job fencing, browser flush
  control, field-aware Asset rebinding, normal bundle validation, and
  authenticated download;
- `5631719` (`ato-pwa`) — separate **Export Ato App…** action with App-only or
  saved-data inclusion, thin/Standard/offline profiles, progress, cancellation,
  and verified download.

The PWA flush ACK carries only revision/generation. Its bridge accepts an exact
parent origin rendered from the app proxy's `frame-ancestors` configuration;
the intentionally absent referrer is not used. Capture rechecks browser state,
Data heads, Asset metadata, and each Asset body's SHA-256. Origin Instance,
Resource, and Asset IDs are excluded from K. JSON/localStorage locations are
explicit bindings, including a flag that distinguishes a plain string from a
JSON-encoded root string; no regex replacement of opaque data is used.

An idempotency reservation now has a bounded capture lease, so concurrent
retries do not write the same temporary object keys and an expired capture can
be reclaimed. Validator claims remain fenced. The produced output is placed
into the existing Capsule quarantine and becomes downloadable only after the
normal Rust validator succeeds. The output-to-validation transition and
export `validating` state are one D1 batch. Temporary capture objects are
removed on capture failure and terminal bundle validation.

Local verification passed:

```text
ato portable unit tests                         42 passed
ato CLI portable integration                    10 passed
ato portable/CLI clippy (-D warnings)           passed
ato-api typecheck                               passed
ato-api focused export/snapshot/import tests    49 passed
ato-pwa typecheck                               passed
ato-pwa focused export tests                     4 passed
ato-pwa production build                        passed
```

Negative coverage includes an unbound alias, wrong capture object set/size,
same-size Asset-body tamper with temporary-object cleanup, stale state fence,
live/expired capture lease behavior, output digest mismatch, claim expiry,
idempotency conflict, unresolved Asset binding, and user cancellation.

This remains a local implementation checkpoint. Migration 0272 was not applied
to staging, no service or PWA was deployed, and no browser export/import
roundtrip is claimed. Hosted offline export can use an already embedded,
verified OCI archive but does not synthesize a missing archive from a remote
registry. Filesystem state, portable Bindings, User Runner placement, and
authoring integration remain later increments.
