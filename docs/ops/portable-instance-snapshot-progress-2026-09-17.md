# Portable Instance snapshot progress — 2026-09-17

## Scope

Repository: `ato-run/ato`  
Branch: `feat/portable-instance-snapshot`  
Stacked on: `feat/portable-durable-local-instance` at `1a07ea20`

Implementation commits:

- `c55b1dea` — v4 snapshot object, Contract binding, closure validation, and
  exact runtime observation
- `123c4949` — durable local materialization, independent Asset IDs, snapshot
  save, and `ato app export`

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

## Verification

Passed:

```text
cargo test -p ato-objects -p ato-formation -p ato-portable-application \
  -p ato-cli --lib --tests

ato-cli unit                         27 passed
ato-cli portable integration         8 passed
ato-formation unit                  64 passed
ato-formation integration           66 passed
ato-objects                         30 passed
ato-portable-application            34 passed
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

## Negative coverage

- v3 snapshot extension is rejected.
- Missing snapshot content is rejected.
- Content with the wrong digest is rejected.
- Materialized Asset tamper is rejected before use.
- Snapshot K without an exact runtime restore observation fails with
  `instance_snapshot_missing`.
- An active Run cannot seal a new snapshot.
- Replacing saved data changes K, preserves D, and removes the old unreachable
  snapshot/resource objects.

## Not complete

This is the object/K/local-storage boundary, not the complete saved-data
roundtrip acceptance.

- No browser-state flush/capture is connected.
- No Hosted Data Resource or Asset restoration is connected.
- No PWA export/import UI is connected.
- No field-aware `ato-asset://` alias resolution is connected.
- No runtime Adapter installs the restored resource into an Application yet.
  Therefore a snapshot-bearing Run intentionally does not report
  `fully_satisfied`; bundle validation or local unpacking is not substituted
  for runtime evidence.
- No filesystem state snapshot is included in this increment.
