# Portable Instance snapshot progress — 2026-09-17

## Scope

Initial snapshot for this record:

- `ato-run/ato` — `feat/portable-hosted-export` at `d9617096`
- `ato-run/ato-api` — `feat/portable-hosted-export` at `614cba90`
- `ato-run/ato-pwa` — `feat/portable-hosted-export` at `ae08f46`

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
- `f4e8a92e` (`ato`) — validate bounded canonical filesystem snapshot tar
  objects and bind them to the declared state slot in K
- `3ceffe4f` (`ato`) — add the write-capable Portable Notes Process/OCI fixture
- `f4988ee0` (`ato`) — run writable OCI mounts as the owning host user
- `be92745a`, `b598c481`, `614cba90` (`ato-api`) — dispatch declared Hosted
  filesystem state, restore/capture revisions, and permit server-side dynamic
  capture without a browser-state bridge
- `ae08f46` (`ato-pwa`) — try server-side state capture before requiring a
  browser flush
- `cfad15d8` (`ato`) — restore, mount, capture, restart, and export one durable
  local filesystem state for Process and OCI Derivations
- `d9617096` (`ato`) — reject browser/JSON/Asset snapshots on dynamic routes
  instead of claiming that filesystem restore satisfied unrelated state

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
  preserves immutable Asset bytes, and installs a new K.
- A declared `ato.state.filesystem@1` slot now has an Instance-owned working
  copy. Import safely extracts only canonical directories and regular files;
  symlinks, traversal, special files, non-canonical metadata, and over-limit
  archives fail closed. Stop creates a deterministic tar after the workload is
  quiesced and installs the next K without changing either D.
- Durable local Process uses a Linux bubblewrap mount namespace so the same
  host working copy appears at the declared guest path. Durable local OCI uses
  the same working copy as a Runner-managed writable bind mount. macOS Process
  and Docker Desktop OCI remain explicit admission failures rather than
  weakening mount or network isolation.
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
ato-portable-application            58 passed

cargo test -p ato-cli
ato-cli unit                         25 passed
ato-cli integration/doc             30 passed
  of which portable integration     11 passed
```

Passed on the final feature head:

```text
cargo clippy -p ato-portable-application -p ato-cli \
  --all-targets --no-deps -- -D warnings
cargo clippy -p ato-cli --all-targets -- -D warnings
```

Rust/Clippy 1.96's `manual_is_multiple_of`, `items_after_test_module`, and
`redundant_closure` findings were corrected idiomatically rather than
suppressed. Cross-platform CLI integration now accepts exactly two outcomes:
a compatible host runs the selected Python 3.12 D and fully satisfies K; a host
without that pinned capability must return the explicit admission error and
must not emit a receipt. This is implemented in ato `28d8718b`; the remaining
Rust 1.96 `useless_conversion` finding is corrected by `ad752888`.

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
  evidence. Dynamic filesystem state is covered by deterministic
  capture/restore unit tests and native Linux Process/OCI acceptance below.
- A stale local Run token cannot seal a new snapshot; a state POST without the
  derived active-Run token receives HTTP 403 and leaves state unchanged.
- Filesystem capture rejects symlinks and non-regular entries. Restore requires
  an empty Instance-owned destination and never extracts traversal paths.

## v0 closure status

Browser state, JSON Saved Data, Instance Assets, and one declared filesystem
state complete the Hosted → local → Hosted roundtrip. The v0 acceptance items
that were open at the first checkpoint are now closed in the later sections of
this record and in
`portable-dependency-export-progress-2026-09-16.md`:

- portable Binding declaration and receive-side manual rebinding were accepted
  without transporting or retaining credential values;
- an explicitly selected User Runner was accepted without Managed fallback;
- strict `ato.capsule/2` authoring and `ato pack` produced the existing
  canonical Contract/Application/Derivation objects;
- the 120,583,935-byte v4 offline Datasette file passed the PWA direct-upload,
  Hosted OCI, and real browser flow;
- that exact file then passed local OCI verification from an empty private
  Docker store in a network namespace with no external route.

The final authoring/selection revisions were ato `872abc91`, API `a283adba`,
and PWA `fc2f531`. The isolated offline harness and failed-launch cleanup were
completed by ato `4d9f75af`. The API and PWA revisions were deployed only to
staging; no production deployment, production migration, or feature-flag
change was performed.

This is the bounded Portable Application v0, not an assertion of arbitrary OSS
support. It intentionally supports one Application Surface, one serving
process, explicit inputs/Bindings, at most one declared writable filesystem
state, and N explicitly selected Derivations. It does not infer arbitrary
directories, capture process memory, transport host kernels/runtimes/drivers,
or provide an automatic Planner or fallback. Those are later-version scope,
not incomplete v0 acceptance items.

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

## Dynamic filesystem-state roundtrip acceptance

The write-capable fixture is `samples/portable-stateful-notes`. It is one HTTP
application backed by SQLite in the declared `data` state slot mounted at
`/data`, with both a Python Process D and an OCI D:

```text
base bundle SHA  sha256:029cb5ebde4a3c03320243c0dfab88b21f49ccf9d1fbc9804b3560ed59384f32
base K           sha256:470ac469c9af31c7af25fefb2111ca80ca08a6435071c2cbdb6ef788d377885d
Python D         sha256:f8331b5349758a209b3c0419c860236db6dd1a60094c6ba158a709a91eb9555d
OCI D            sha256:03965b697def12fe8dec54dd23a8ce9f11a10573b70cc655c558f7e3b0c02c08
OCI image        docker.io/library/python@sha256:1c44018d7eb40488f29e7c6ad4991d3200507e14dca71b94fe61011815e98155
platform         linux/amd64
```

Staging ran API `614cba90` as Worker version
`4a6726c3-c36f-415d-b51d-1485b821bdf3`, PWA `ae08f46` as Worker version
`229f6d51-22fe-4a0a-b4f7-a110b4ab83b1`, and both Runner slots with the
`f4988ee0` worker binary whose SHA-256 was
`54b0dc09de7c9ead84a0a54956b1c0f1c56bd8b3af3c693b654ec066880d23b5`.
No production deployment was performed.

The first Hosted Python Instance was
`cinst_01M2P762S2RKJ3W1C6BYWD6P6K`. The real Surface accepted the note
`saved on Hosted Python`. PWA **App + saved data** quiesced
`run_01M2P762VZY5181CG3SVRB3HXV`, released its writer, and committed revision
`isrev_01M2P82Y0TZNK3RSFTSTEB3ZQH`. Its revision digest equals the embedded
filesystem resource digest:

```text
file          /Users/egamikohsuke/Downloads/Portable-Notes.capsule
size          48,895 bytes
bundle SHA    sha256:0753c059f5261980d92806dec91400fba2553e130ca0d3b34d84ff33c9cfadcc
K_saved       sha256:dadd3184057263016063eb9a13acd81e97dc3b63e8a5596f2a410216a31ab9b7
snapshot      sha256:03872a47a1e963f11eb78d05a12bfa9135d3549d6eaab810595d1941204f8703
state digest  sha256:794e811842ccb7e4840e49f996eb605fad7305fd3be98e91d262686d6c567f22
```

The exact file was imported as Hosted OCI Instance
`cinst_01M2P8AZYJTYQCHJWQCKRVEMXY`. Its persisted receipt selected the OCI D,
bound Run `run_01M2P8B0N3CSQK9693HV0MMMQE` and lease
`01M2P8B0TQTZQ2VB2SNT86GAHW`, and reported both the snapshot and HTTP
observations satisfied with `fully_satisfied=true`. PWA showed Saved data
Restored and Surface Ready, and the real Surface displayed the Python note.
After adding `continued on Hosted OCI`, a second PWA export produced:

```text
file          /Users/egamikohsuke/Downloads/Portable-Notes (1).capsule
size          48,895 bytes
bundle SHA    sha256:26db31ca98c5ec8635ba47e5d5cbedfa945581bda22c45d86ff3fd5822ed6f18
K_saved       sha256:110959e7227bcb5e102d36c962e70aa78a9e8f42dde0c89fadfc2c6d3aac8dc2
snapshot      sha256:a066f03adf1757b75208e774bdc83cb71bd4da48117605d6b55647ab74fbeca9
state digest  sha256:4cffee49fc67903a478d6ecc51e6e5344c47e72a20c3238949dc76e415fcbbda
```

Native Linux host `ubuntu-sugamo` then imported that exact file into durable
Process Instance
`linst_c43e675e1d3fd04536e51d0093da664bf16f46d914c741804a76f41b3e28faf8`.
Run `lrun_8a6c7c121b2f891ab063aecbbb89b3b2e15f9d9bc9b03676e7ed90e234d0935f`
restored both notes under bubblewrap, and the receipt recorded Python 3.12.7,
the Process D, exact input SHA/K, snapshot evidence, and
`fully_satisfied=true`. After adding `continued on local Process`, stop minted
the next K. Run
`lrun_9016228e4120c720257eb053bb09e65fb726ddb4a677302986bb69f4d175425c`
restarted the same Instance and displayed all three notes. Export produced:

```text
bundle SHA  sha256:e8ef701f148ba8f3bf83210168ed132d74daf93fe41910ad121f4d534ae75053
K_saved     sha256:3ecd117e87bf251cc35ddf10c7d5dbe838624d87b80325b8a6ddfae4e128a480
snapshot    sha256:38f5350eb44e202175d49165f8746c589606810d98b17c1bb95695776bcb4ba7
Python D    unchanged
OCI D       unchanged
```

That file was imported independently into durable OCI Instance
`linst_677ef2619566f7c4a1c466e053bec1a6fa8bc7f7d8a9bcda70b6c224365b755d`.
Run `lrun_d724e6e9b116eb24a7989f26ce6c9418d5cbacb267fbdbbe3288844b83d1dde3`
restored all three notes with the OCI D and a fully satisfied receipt. After
adding `continued on local OCI`, stopping and starting Run
`lrun_f417d1bae5dcc6ac3116eab3165c8f442fa184c6b022c824ee5ced5c7498f329`
displayed all four notes. Final local export:

```text
file          /Users/egamikohsuke/Downloads/Portable-Notes-roundtrip.capsule
size          48,895 bytes
bundle SHA    sha256:e30c17a3ae0be9ef8e791c8ff01a8dd2ac8bf2f5699c217dcfad6fc612fa4816
K_saved       sha256:83f109f3dae428885fc50cee19cdf75e98159abd90a14281a97b53cf7cd29bd2
snapshot      sha256:1f9aac6d984886d9c2bb05935c382e453468072aff73e7d73d7cf22f9145ca4b
Python D      unchanged
OCI D         unchanged
```

Finally that file returned to staging as independent Hosted Python Instance
`cinst_01M2P98TEM2CSWSF3WV1X4K0ZW`, Run
`run_01M2P98VHJWNTHDT2V7WHBTK03`, lease
`01M2P98VP281E2N3BGKV7YYC68`. The persisted receipt exactly matched the final
bundle SHA, K, Python D, and snapshot, with all observations satisfied. PWA
showed Contract Verified, Saved data Restored, and Surface Ready. The real
Surface displayed all four notes in order.

The four dynamic Instance IDs are distinct. Each edit changed the snapshot,
K, and transport SHA, while the two declared DerivationRefs and ApplicationRef
remained unchanged. macOS restored the same state bytes during import, but
Process admission failed because bubblewrap is Linux-only and OCI admission
failed because Docker Desktop keeps the isolated bridge inside its VM; neither
case silently selected the other D or weakened isolation.

## Portable Binding acceptance

Portable Applications can now declare required runtime Bindings without
placing recipient values in the bundle, K, or D. Rust maps the declared
`service` Binding to the stable `ATO_BINDING_SERVICE` process/container
environment name. CLI and durable local starts require an explicit `--bind`;
Hosted import prompts for the value before allocating the Run. The API seals
the value to the exact Run/lease/K context, places only a grant reference in the
launch specification, and exposes the plaintext once on the authenticated
Runner channel. Process and OCI launch environments receive the resolved value;
the OCI env file is removed immediately after `docker run` returns.

The final fixture keeps the Contract observation at `/health` and also serves
the Surface root so **Open App** is useful without changing K:

```text
file          .tmp/portable-binding-echo-v2.capsule
size          5,441 bytes
bundle SHA    sha256:32875cc24dd6c3116ab2c93e772b22f28460d0ad9bd2538521cde285555ba6d7
K             sha256:d9fa726e80f46d036801f7333e3b6d5a7791a8f2e393bf82798a40e47c79103f
Python D      sha256:9f21b0144f6e51fa54ac67443c5339b984ff62238217e3229f925ee2bbfb3465
OCI D         sha256:eb631e9fc463120fea26d77ee5173e0eb30c25e0cf543ebb150d3c433320b247
```

The exact bytes passed all four routes on 2026-09-17:

| Entry | D | Result |
|---|---|---|
| Linux CLI | Python Process | `binding-health=satisfied`, fully satisfied |
| Linux CLI | OCI container | `binding-health=satisfied`, fully satisfied |
| staging PWA | Python Process | Instance `cinst_01M2PDJH18EWFQ63YCF52FN1TB`, Run `run_01M2PDJH6D61VAW7BVJGEFJCJ7`, lease `01M2PDJH6D08NPDMW5S3VJNQBA` |
| staging PWA | OCI container | Instance `cinst_01M2PDVANHV57MSPRHQ6FVV4EP`, Run `run_01M2PDVAT0D74SZ8FWFTJKB6XJ`, lease `01M2PDVAT01Q7SHZ5WA6KW9PSJ` |

Both Hosted receipts persisted the exact K and selected D, one required
observation, `outcome=satisfied`, and `fully_satisfied=true`. The PWA displayed
the required connection form before import, then Contract Verified and Surface
Ready. **Open App** produced `binding-ok` in a real browser for both Python and
OCI. The two synthetic acceptance values had zero matches in the lease command
and verification receipt, and `capsule_hosted_binding_grants` contained zero
rows for both Runs after redemption. Missing and undeclared Bindings fail before
runtime allocation; a second redemption returns Gone. Local durable metadata
and re-export were also scanned without finding the supplied value.

The first browser attempt exposed two operational boundaries rather than being
hidden: the old validator rejected the new schema, and the staging Worker was
already at its 128 text-binding limit. The validator was updated from the same
Ato source. The relay now prefers a dedicated 32-byte key but, when that binding
is absent, derives an AES-256-GCM key from the existing Run-control root with
HKDF and binding-specific salt/info. No existing secret or feature flag was
removed. A rejected bundle row remains immutable; the PWA reuses the digest key
for ready bundles and creates a fresh attempt key only after confirming the
prior rejection was `validator_failed`.

Executed/deployed revisions:

```text
ato source HEAD       07c9788a (runtime/validator binary ancestor 2ad762ba)
API                   9a00ebeb
PWA                   3372bad
API Worker version    61c7127d-eab8-4ca6-89b2-c5f7c1fb61a5
PWA Worker version    7a95c54d-d329-4cd8-9c6f-150a9b779cf7
Runner binary SHA     c7ba0bd05e93a4d02bcd69b59e0c4db70f782c7d09433d2a8e72c5abf8f79783
Validator binary SHA  2e65e6a8f86852a159674413e09c5dc0906a0e02c9ab7f9fd9af18031ccb46de
```

The acceptance Runs were explicitly stopped. Both leases acknowledged
`stopped`; the OCI container and per-lease workspace were gone. Production was
not deployed.

## Explicit User Runner placement acceptance

Portable dynamic import now accepts a placement request that is separate from
K and D:

```json
{"kind":"user_runner","runner_id":"01M1JVXA4DX08ZR0VJQXV3BZFA"}
```

The API admits only a runner owned by the importing account which is active,
not drained, online, has a free slot, supports `runtime_launch`, advertises the
selected execution ABI and platform, and can publish the required Web Surface.
The chosen runner ID is persisted on the idempotency reservation and import;
retry and wake reuse it. A rejected User Runner request does not fall back to a
Managed Runner.

Staging acceptance used the same Binding fixture bytes and explicitly selected
`p3-acceptance-sugamo-2` in the PWA's **Run on** controls. The result was:

```text
Instance        cinst_01M2PGTPSXDSAZ12RFSW4FBXYS
Import          pai_01M2PGTPXNSF5PY3KW52FGVXMD
Run             run_01M2PGTQ1019X0T00F5A8328F6
Lease           01M2PGTQ10VFNXSXXWW72466TN
Runner          01M1JVXA4DX08ZR0VJQXV3BZFA (user_managed)
Placement       external-runner
bundle SHA      sha256:32875cc24dd6c3116ab2c93e772b22f28460d0ad9bd2538521cde285555ba6d7
K               sha256:d9fa726e80f46d036801f7333e3b6d5a7791a8f2e393bf82798a40e47c79103f
Python D        sha256:9f21b0144f6e51fa54ac67443c5339b984ff62238217e3229f925ee2bbfb3465
receipt         verified, 1 observation, fully_satisfied=true
Surface         https://cinst-boru2sqt2mcf36ni.stg-app.ato.run/
browser result  binding-ok
```

Both `portable_application_import_requests.placement_json` and
`portable_application_imports.placement_json` contained the exact User Runner
ID, and the resulting lease was assigned to that same ID. PWA showed Contract
Verified and Surface Ready; the public Cloudflare path returned the real
workload response. The stop control signal was observed within one second, and
the lease and Run both reached `stopped`.

The acceptance temporarily borrowed the already-routed
`s2-rstg002.ato.run -> 127.0.0.1:8422` staging ingress. Afterwards the temporary
credential and work root were removed, the original runner token hash was
restored, the acceptance ingress rows were revoked, the prior Step 10 ingress
was reactivated, and the prior Step 10 service was returned to its pre-test
auto-restart state. Both Managed staging slots remained active throughout.

Executed/deployed revisions for this slice:

```text
Ato                    d1b53f7e
API                    56dca1a5
PWA                    72800a7
API Worker version     f99b0e30-094a-483e-9200-3962d1cb40b7
PWA Worker version     eff90245-9604-401f-a5db-3cef2104729d
Runner binary SHA      919c94e3a80483c2b967d0832cc94601c8e97719f5a499c1ae605386ead4838b
```

Migration `0273_portable_user_runner_placement.sql` was applied only to
staging. No production migration or deployment was performed.

## Formal authoring and exact same-kind Derivation selection

`ato.capsule/2` is now a separate strict grammar from `ato.capsule/1`. The new
`ato pack` path compiles its bounded portable v0 shape into the existing Rust
canonical Contract/Application/Derivation objects. The authoring manifest and
route labels are not runtime workspace content or identity inputs. Renaming
only the labels produced byte-identical output in the automated test.

The committed `samples/portable-multi-process-authored` fixture declares two
Python 3.12 Process routes with different environment values and one common
HTTP Contract. Packing it once produced:

```text
file        .tmp/authoring-acceptance/portable-multi-process-authored.capsule
bundle SHA  sha256:5cc3419d68b2d2a4ae9002670ccb08f8140762eafd3eef5d2dbc7f09fe821033
K           sha256:294c2312a3a6e90dab2030edcf1debce5fe7372f6ffe6220662d5c0f8523ef7c
Process D A sha256:4ddf1df9cd1d324603df3e8834eb4e796b373a908890bfab028f5fea984f0359
Process D B sha256:8d1fee1f81a107d8734a0fc2774633422ac0df44cee64ba7b315449095e15363
```

CLI ran those exact bytes once per D. Both receipts used target `cli-local`,
the same bundle SHA and K, the explicitly selected D, one `satisfied`
observation, and `fully_satisfied=true`.

API `a283adba` exposed validator-owned runtime labels for every candidate.
PWA `fc2f531` stopped using the first route of a realization kind: one
candidate is selected automatically, while two or more require the person to
select a concrete DerivationRef. Staging deployed them as API Worker version
`41274372-5c62-4455-95db-849a7b21a3b5` and PWA Worker version
`a71e54d6-ed8b-4dbf-888d-9b3bce1abb3a`. API health returned 200. No migration,
production deployment, or feature-flag change was performed.

The PWA showed two initially unselected choices:

```text
Python 3.12 sha256:4ddf1df9…
Python 3.12 sha256:8d1fee1f…
```

The second candidate was selected. The resulting acceptance evidence was:

```text
Instance        cinst_01M2PK30S274TQWWK561R68XYX
Import          pai_01M2PK30V6XQ1RXRQYANFSESHP
Bundle          bnd_01M2PK21S4CPVMND02D0BAWY6E
Run             run_01M2PK30X61VG5T78WZHQXF0W8
Lease           01M2PK30X6XZYYEWN3QR084E2J
Runtime route   rrt_01M2PK31E82MBAX02RGJ749M9R
selected D      sha256:8d1fee1f81a107d8734a0fc2774633422ac0df44cee64ba7b315449095e15363
runtime         /opt/ato/toolchains/python/3.12.7/bin/python3
version         Python 3.12.7
receipt         root=satisfied, fully_satisfied=true
Surface         https://cinst-vmsfmrmimao2pd26.stg-app.ato.run/ (Ready)
```

The persisted receipt matched the local bundle SHA and K exactly and observed
body SHA-256
`ef1f781ce1776a53a072bcaf7630be1affca3326a980a03af3942217fd273d30`.
PWA displayed Contract **Verified**, the exact second D, and Surface **Ready**.
The in-app automation browser refused direct navigation to the wildcard app
host with `ERR_BLOCKED_BY_CLIENT`; therefore that attempt is not recorded as a
successful browser body check. The Hosted verifier did read the real Run HTTP
body, and the runtime route was independently `ready`. Existing earlier
Datasette/Binding/Stateful Notes acceptance remains the browser-level Surface
evidence.

After capture, a scoped stop signal was sent only to this Run/lease. The Runner
acknowledged within two seconds: Run and lease became `stopped`, and the
runtime route became `detached`.

Executed source revision for the authoring slice:

```text
Ato 872abc91
```

Regression evidence: Formation 67 tests, portable Application 61 tests, and
the CLI test suite passed. A new CLI integration test packs the v2 fixture and
runs both explicit D references to full Contract satisfaction.
