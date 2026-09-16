# Portable Instance snapshot progress — 2026-09-17

## Scope

Repositories and branches:

- `ato-run/ato` — `feat/portable-hosted-export` at `8ea240be`
- `ato-run/ato-api` — `feat/portable-hosted-export` at `cce1994c`
- `ato-run/ato-pwa` — `feat/portable-hosted-export` at `5631719`

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
- `37058059` (`ato`) — reject isolated OCI HTTP endpoints on Docker Desktop
  instead of silently weakening network isolation
- `cce1994c` (`ato-api`) — wake an imported portable Application with its exact
  verified Derivation and workspace instead of the generic cold-wake path
- `8ea240be` (`ato`) — restore browser state and Assets into a durable local
  static Surface, capture edits on fenced stop, and emit the next saved K

Draft pull requests:

- `ato-run/ato#1358`
- `ato-run/ato-api#655`
- `ato-run/ato-pwa#396`

API/PWA and migrations 0270–0272 were deployed to staging for acceptance. No
production deployment or production flag change was performed.

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
- A durable local static Run now injects the restored browser state before the
  application script runs and serves rebound Instance Asset bytes from the
  same-origin `/__ato/assets/<asset-id>` path.
- The local state bridge is scoped to the active Run with a derived write
  token. Missing-token cross-origin style POSTs cannot mutate captured state.
- Stopping a local Run captures the exact browser state behind the active Run
  token, converts only declared Asset binding fields back to portable aliases,
  preserves immutable Asset bytes, and installs a new K. Process/OCI snapshot
  restore remains fail-closed until a state Adapter exists; it is not reported
  as restored merely because bytes were materialized.
- Local Asset IDs now use the same `ast_<Crockford Base32>` lexical shape as
  Hosted Assets while remaining independently minted per Instance.
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
cargo test -p ato-portable-application
ato-portable-application            49 passed

cargo test -p ato-cli
ato-cli unit                         25 passed
ato-cli integration/doc             30 passed
  of which portable integration     11 passed
```

Passed:

```text
cargo clippy -p ato-portable-application -p ato-cli \
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
- CLI integration confirms a restored static Run emits exact snapshot
  evidence, while a snapshot-bearing Process Derivation fails before it can
  claim restoration.
- A stale local Run token cannot seal a new snapshot; a state POST without the
  derived active-Run token receives HTTP 403 and leaves state unchanged.

## Not complete

Browser state and Instance Assets now complete one full Hosted → local → Hosted
roundtrip. The remaining v0 work is deliberately narrower and still open:

- one declared filesystem state mount for Process/OCI, including quiesce,
  content-addressed capture, restore, and lifecycle cleanup;
- portable Binding declarations and receive-side manual rebinding without
  credential transport;
- explicit User Runner placement for portable Runs without Managed fallback;
- the normal `capsule.toml` authoring path for the supported v0 shape;
- empty-cache plus real outbound-network-block acceptance for the offline
  Datasette profile;
- large v4 direct-upload acceptance for the approximately 120 MiB offline
  Datasette bundle.

The local static bridge intentionally does not make Process/OCI snapshot
claims. Dynamic filesystem state remains a separate Adapter increment.

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
ato portable unit tests                         49 passed
ato CLI portable integration                    11 passed
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

Migrations 0270–0272, API `cce1994c`, PWA `5631719`, and validator
`83977e97` were applied to staging for the acceptance below. Hosted offline
export can use an already embedded, verified OCI archive but does not
synthesize a missing archive from a remote registry. Filesystem state,
portable Bindings, User Runner placement, and authoring integration remain
later increments.

## Hosted OCI Surface acceptance

The existing Datasette OCI import was cold-woken through the exact verified
portable import rather than the generic Instance wake path:

```text
Instance   cinst_01M2MEX5Q8TX4VP45Q2QN32CXM
Wake       wake_01M2P0BERFNMCXZBGE90ZEGKCW
Run        run_01M2P0BEV9T9C50AZ7S9GMWY2M
Route      rrt_01M2P0BF03CS4TE10Z15XDNXZ7
Lease      01M2P0BEXD9CRZYA3GF0HCZREV
```

The public Hosted Surface no longer returned 524. Browser acceptance displayed
the Datasette catalog, rows `A/2` and `B/5`, and SQL result `total=7`. Contract
verification and public Surface readiness remained separate states.

The PWA app-only export was also run through both local Derivations. The exact
file was `/Users/egamikohsuke/Downloads/Datasette-catalog.capsule`:

```text
size          7,000,996 bytes
bundle SHA    sha256:e23f5ed24504ca8d953df12b1390c77825a6a785ce00f87c135f694baac3deeb
K             sha256:d4bb7e9ae1be0f6b884d0d58561d7f974092de447da3eea9e2ffe5279b004a19
Python D      sha256:94ab606ee88d41da3af332e72d5da151fd0920a3aadc9c8c94aefaa80518b21f
OCI D         sha256:d3e7428a6c75e7d5726319ca61f06631e1811a698b2dd16b531029038dfd701f
```

Python passed locally on macOS. OCI was admitted and passed on the native Linux
host `ubuntu-sugamo` with the same bundle SHA, K, and three required
observations. Docker Desktop fails early with a reasoned admission error because
its internal bridge exists inside the VM; the implementation does not relax
network isolation or fall back to Python.

## Saved-data roundtrip acceptance

The writeable fixture is `samples/portable-todo-assets`: a static Todo app that
uses browser localStorage and a real Instance Asset upload/read path. The base
Application was imported to staging as
`cinst_01M2P1W8CW5KB8H8QFV38QMJ4V`. Two Todo items and `ato-logo.png` were
saved through the actual app UI, then **App + this app's saved data** was
exported in the PWA.

Hosted export:

```text
file          /Users/egamikohsuke/Downloads/Portable-Todo.capsule
size          436,789 bytes
bundle SHA    sha256:229eca58493dd4cd9ad8b19e4c9ccb9d67af48abe91837738b5ccf0915d8c9fa
K_saved       sha256:eabfc847ee0622c1dfab6a890a592dd068cc612826da7d5756f7df4f82b97df3
D             sha256:581f14f199ed8ff1aa318c54d38e6cb8e810e27d7bc8bde698d41aca25131e9b
snapshot      sha256:6899b7482ae40b4b4e364d4749271882a9df636e3827f11e44072462e85522b2
```

That exact file was imported into a local durable Instance independent of
ato.run:

```text
local Instance  linst_57865b6064e1fc31cd6229b747e443c51be210228f04be5bf6385ba8bfd58850
local Run 1     lrun_19318c3e96f23abf66ccd0d4c0707b430a61e16e4999971b1349a7bd387e0f72
local Run 2     lrun_41966dbfdd9798200c56db4bdde73ad790e4b47de9c6e597a4246134ce56ce6c
```

The first local Run displayed both Hosted Todo items and the image. A third Todo
`third local todo` was added. Stop minted a new saved K and snapshot while D
remained unchanged. Restart displayed all three Todo items and the image, and
its canonical receipt had three satisfied observations with no deferred or
failed outcome.

Local re-export:

```text
file          .tmp/portable-todo-evidence/local-edited.capsule
size          436,817 bytes
bundle SHA    sha256:073be29c9b1e304442829ca6bb9f1571893cd47e99d79923028f977291d021b1
K_edited      sha256:04ecf7df50e1d1ac038717ed5c0e19f8a8c1281877fe85d5e7bb80a0238bae7c
D             sha256:581f14f199ed8ff1aa318c54d38e6cb8e810e27d7bc8bde698d41aca25131e9b
snapshot      sha256:1aec625f93e636541ff18564a143f024e4df44baec1936321c9be53f5f3d405b
```

The local-edited file was then imported to staging as a new independent
Instance `cinst_01M2P33RSXY37X68R513Y120Y4`. PWA Ready displayed Contract
Verified, Saved data Restored, and Surface Ready. The app UI displayed all
three Todo items and the image. The persisted Hosted receipt was
`fully_satisfied=true` with three observations and exactly the same bundle SHA,
K, D, and snapshot ref shown above.

Identity/rebinding assertions:

```text
origin Hosted Asset  ast_01M2P1XPFGXYSYT5STVPFGZARN
local Asset          ast_3E27NQQYMF10A40CK6S5HAB9M5
new Hosted Asset     ast_01M2P33RYSZAK7J2WW0GTQ0A2M
Asset body SHA       sha256:2c96bb4cd78b00629bca2498d581fcf1fbeda3f6012105659795602cd0d9e91e
```

All three Asset IDs differ. Staging D1 records both Hosted Assets as available,
318,723-byte objects with the same SHA-256; the local body also hashes to the
same value. The three Instance IDs differ, saved-data changes changed K, and D
remained constant. No credential or signed URL was transported.
