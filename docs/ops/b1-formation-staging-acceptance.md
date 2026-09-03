# B1 Formation — staging acceptance record

Executed 2026-09-03 against real staging. No fake control plane: real
`staging.api.ato.run`, real D1, real R2, real `runner_leases`, a real Connected
Realization Worker and a real build sandbox on `ubuntu-sugamo`.

`B1_BASELINE`: ato `f637974e` · ato-api `c4d30f9` · ato-pwa `0c1785e`

## The claim

> A pinned GitHub source is formed into a canonical ComputeSchema and an
> immutable workspace, and P3 launches a Run from that Schema alone.

It holds. `POST /v1/internal/compute-instances/:id/runs` now takes `{}`.

## Environment

| | |
|---|---|
| API | `ato-store-staging` `da29f393-aa56-489b-9c3b-4ceae55d8a2f` |
| Migrations | `0195`–`0201` |
| Source | `ato-run/fixture-fastapi-sqlite-process` @ `922b112f63f58678a807aff0920690d113331199` |
| Workspace materialization | `sha256:95818fc8ece974f6849fcc77f3b3f0433e513f92d0e9a6ef7766771512aab1be` (112 MB) |
| ComputeSchema | `csch_01M1KE6Y4QNCPC1TD3351YW1N6` |
| Instances | `cinst_b1_a` / `_b` / `_c`, all on that one schema |

## Results

| | Scenario | Result |
|---|---|---|
| A | contract | **PASS** — forbidden fields (incl. nested `secret_value`), mutable ref, escaping subdirectory, `publish_enabled`+network, `oci_image`, unknown protocol: all typed refusals against the real API |
| B | source pinning | **PASS** — a full commit is required; `main` and a short SHA are refused as `ATO_ERR_FORMATION_SOURCE_NOT_PINNED` |
| C | closure identity | **PASS** (unit) — the same tree from two different archives yields one closure; the closure is neither the archive digest nor the tree digest |
| D | idempotency | **PASS** — a resubmitted key returns the same job |
| E | stale attempt | **PASS** — `409 formation_attempt_superseded`; a duplicate completion is `409 formation_result_already_accepted` |
| F | build sandbox | **PASS** — source read-only, output writable, host sentinel invisible, env not inherited, denied network genuinely fails, allowed network reaches the index |
| G | Static Formation lane | **PASS** — a pinned source formed into a manifest/receipt/blobs bundle by the canonical materializer; schema registered as `static_web`, no process realization, **0 Runner runs** |
| H | Python Formation → Run | **PASS** — unattended worker: claim → pinned fetch → build → publish → schema; then `{}` in, `{"status":"ok","db_path":"/data/app.sqlite"}` out |
| I | state continuation | **PASS** — three Runs of one instance, fences 1→2→3, revisions chained, `note-A` survives to the third |
| J | tenant isolation | **PASS** — B and C start empty on the same schema; B never sees A |
| K | existing artifact compatibility | **PASS** — both Static instances open unmodified, P0 bridge live, the untouched one has **0 runs** |
| L | rollback | **PASS** — off → shadow → primary-unallowlisted (degrades to shadow) → primary-allowlisted → **off**, then an existing instance still runs and its state is intact |

## The build, in full

```
pinned commit  922b112f
  -> source fetched, extracted read-only at /src
  -> CPython 3.12.7 provisioned to /opt/ato/toolchains  (NOT the host's 3.14)
  -> venv --copies --without-pip, libpython vendored beside it, ensurepip
  -> pip install -r requirements.txt
  -> import fastapi, uvicorn, pydantic, main   ->  OK, fastapi 0.115.6
  -> packed deterministically -> sha256:95818fc8…  (112 MB)
  -> R2 -> FormationResult -> ComputeSchema
  -> Run, with no caller-supplied artifact
```

## Eight defects the staging run found

None of these would have surfaced from reading the code.

| Symptom | Cause |
|---|---|
| `cannot exec /bin/sh` | usrmerge: `/bin` is a symlink into `/usr`, and only `/usr` was bound |
| `Permission denied` on `/bin/sh` | the Landlock policy and the bind list had drifted; now one list |
| `failed to get random numbers to initialize Python` | Landlock lacked the `/dev` bwrap supplies with `--dev`, so `/dev/urandom` was unreachable |
| `error: linker cc not found` building `pydantic-core` | the plan used the host's `python3` (3.14, no wheel) instead of the resolved 3.12.7 |
| `Temporary failure in name resolution` | `/etc` bound wholesale carries `resolv.conf` as a dangling symlink |
| a 471 MB workspace | `install_only` ships a 207 MB unstripped libpython; `install_only_stripped` is 38 MB |
| a venv that cannot start | symlinks cannot cross the artifact format, and `--copies` alone leaves the loader without libpython |
| `exited before becoming ready (exit status: 3)` | the materialized workspace **lost its execute bit** in transport — the same workspace served from the build directory and failed from its content address |

The last one is the one worth keeping: it was only findable by putting a real
Formation artifact through a real Runner.

## The artifact was three times larger than it needed to be

Worth recording, because the mistake generated most of this record's second
half. The workspace VENDORED the whole CPython — libpython plus the standard
library — and that single decision produced, in order:

- 198 MB artifacts (85 MB of which was a `lib64 -> lib` link materialized as a
  copy of the whole `lib` tree)
- past the control plane's 100 MB request cap, so a multipart upload
- past a Worker's 128 MB memory, so a streaming download and a digest check
  moved to the Runner
- a venv that could not start, because `--copies` leaves the loader without
  libpython and symlinks cannot cross the artifact format

`runtime_requirements` already said `python = 3.12.7`, the Runner already
provisions it at a fixed shared path, and the runtime sandbox already binds that
path read-only. The build was ignoring all three.

The artifact now carries the source and its installed dependencies and nothing
else: **11 MB**, down from 198 MB. The launch names the provisioned interpreter
and is told where its dependencies are through `PYTHONPATH`.

## Not done, and not claimed

**No shadow comparison against the existing Static path.** The Static lane runs
and produces a correct bundle, but it has not been run side by side with the
path that produced the current Static instances, so nothing here shows the two
agree. The gateway and semantic comparator are implemented and tested; the
comparison itself has not been performed.

**Untrusted public Python publish is not enabled**, per ADR-018. `uv sync` needs
the network, bwrap cannot express "the package index and nothing else", and the
contract refuses `publish_enabled` together with a dependency-resolving network.
The Python lane is allowlist-only on staging.

**`uv sync` was not exercised.** The fixture carries `requirements.txt`, so the
pip lane ran. The `uv` lane is compiled and unit-tested, not run on staging.

## Production

Unchanged.

## B1-S §1 — Static shadow comparison (fixture C: 2048)

**Verdict: FAIL — 1 unexpected semantic drift, 7 files.**

Same pinned source on both sides:
`gabrielecirulli/2048@478b6ec346e3787f589e4af751378d06ded4cbbc`, no subdirectory.

| | PRIMARY (existing Static path) | SHADOW (FormationServiceV1) |
|---|---|---|
| materialization | `swm_iuxlwt2dqf3kmgcwfw2efj76u2kdq3htvfh4tlrw2q56sgjcp6pq` | `sha256:11a330e4bf1e20fc40fa771b1488e76dba81a33c966c33b8bda726a8391ddea7` |
| manifest digest | `sha256:fd471ae757708841f084b11d777b6bd1782bac7b78cb1696a736eb5cf46e8ba2` | — |
| files published | 27 + injected bridge | 20 + injected bridges |

The pinned source tree holds 33 files. The two paths disagree about which of
them are part of the site.

### Difference 1 — non-servable source files — EXPECTED VERSIONED DIFFERENCE

`.gitignore`, `.jshintrc`, `CONTRIBUTING.md`, `README.md`, `Rakefile`,
`style/helpers.scss`, `style/main.scss`.

Both paths agree these are repository files, not site files. The existing path
drops them; Formation initially REFUSED the whole build on the first one it
could not type. Refusing is not a stricter reading of the same contract — it
turns every repository-root static Compute from "publishes" into "build error"
at cutover. Formation now selects, using the producer's own `media_type_for`
table so selection and production cannot disagree. Resolved; no drift.

No build ran on either side: `style/main.scss` is dropped and the checked-in
`style/main.css` is published unchanged. The existing Static path is a
selection over the source tree, not a build.

### Difference 2 — instrumentation — EXPECTED VERSIONED DIFFERENCE

The existing path injects `__ato/replay-bridge-v0.js`. The Formation lane
injected nothing, because it called `produce_static_web_bundle` directly and
skipped `extract_static_web_output_*`, which is where the browser-runner and
instance-state bridges are applied. A site formed that way would have looked
correct and silently lost its Browser Instance State. Fixed by extracting
before producing. Resolved; no drift.

### Difference 3 — media-type coverage — UNEXPECTED SEMANTIC DRIFT ⛔

Seven files the live artifact serves are absent from the Formation bundle:

    favicon.ico                                  image/x-icon
    style/fonts/ClearSans-{Bold,Light,Regular}-webfont.woff   font/woff
    style/fonts/ClearSans-{Bold,Light,Regular}-webfont.eot    application/vnd.ms-fontobject

`media_type_for` in `ato-materializer-static-web` covers `woff2` but not
`woff`, and covers neither `ico` nor `eot`. The live manifest names all three
media types, so the artifact now serving on staging was produced by a table
WIDER than the canonical materializer's.

This is not an I0 transplant regression: `origin/main`'s table is identical to
nightly's. The wider table belonged to the legacy Builder daemon. The
consequence is that **the canonical materializer cannot reproduce a live
staging artifact** — Formation only inherits that gap by calling it.

Effect on the software's meaning: the favicon disappears and the Clear Sans
faces lose their `woff`/`eot` sources. The `.svg` face survives, so the page
renders in a fallback rather than breaking — a degraded site, not a dead one,
which is precisely the kind of difference a digest comparison alone would have
called "just a different hash".

**The Static lane does not go primary.** Closing this requires widening the
canonical table to cover `image/x-icon`, `font/woff` and
`application/vnd.ms-fontobject`. That changes which byte sequences the
materializer will admit into an artifact identity, so it is the materializer
owner's decision, not Formation's, and not one to make silently to get a gate
to pass.

## B1-S §3 — Shadow side-effect zero — PASS

Measured on staging D1 after the shadow attempt that ran to `Accepted`
(job `fjob_01M1KNNCG4B2078VWVJXVRW9XP`, attempt `fatt_01M1KP7EK0TQ7103PPEZ0AW2NN`,
fence 2, materialization
`sha256:11a330e4bf1e20fc40fa771b1488e76dba81a33c966c33b8bda726a8391ddea7`):

- `formation_results` for the job: **0** — the gateway returned
  `registered: false`, so no result was ever accepted into the registry.
- newest `compute_schemas` row on the compute: `12:44:17Z`, 23 minutes BEFORE
  the shadow attempt. No Compute was published.
- no ComputeInstance, Run, stable URL, Listing or Post was created. There is no
  path to one: every downstream object derives from an accepted result, and
  there is none.

The worker's own line reports the same thing from the other side:
`outcome=Accepted { compute_schema_id: "" }` — the attempt succeeded and named
no schema, which is what shadow means.

## B1-S §4 — Shadow failure isolation — PASS

The FIRST shadow attempt on this job failed outright
(`unsupported static web media type: .gitignore`). Nothing downstream moved:
`formation_results` stayed at 0, the job remained claimable, and the fence
advanced to 2 for the retry. The live artifact
`swm_iuxlwt2dqf3kmgcwfw2efj76u2kdq3htvfh4tlrw2q56sgjcp6pq` was untouched —
shadow holds no writer position over it and never did. A shadow failure is a
shadow failure.

---

# B1-S Static matrix — after the compatibility fix

## Compatibility fix (approved 2026-09-03)

Three extensions added to the canonical materializer, as a live-artifact
compatibility correction, not as new format support:

    .ico   → image/x-icon
    .woff  → font/woff
    .eot   → application/vnd.ms-fontobject

Both halves were widened together — `media_type_for` and
`is_allowed_media_type` — and a test asserts the widening is exactly these
three, refusing `ttf`, `otf`, `map`, `xml`, `pdf`, `mp4`, `md`, `scss` and
extension-less files. The table stays an explicit deterministic list: no MIME
database, no sniffing. Unknown types keep the existing fail/drop policy.

**Identity impact.** Inputs that already succeeded produce the same bytes and
the same digest: none of them contained an `ico`, `woff` or `eot`, because such
an input could not succeed. Only inputs that previously lost those files — or
were refused outright — get new bundle bytes. No existing artifact is rewritten
and nothing is rebuilt.

Two structural fixes shipped with it:

- **The producer takes the extraction proof, not a path.**
  `produce_static_web_bundle` now accepts `&ExtractedStaticWebOutput`. Every
  caller already held one; the Formation lane did not, which is how it reached
  the producer without extracting and shipped a bundle with no bridges. The
  bypass no longer has a spelling.
- **The lane selects rather than refuses.** Files with no servable media type
  are dropped, using the producer's own table so selection and production
  cannot disagree. Refusing them turned every repository-root static Compute
  into a build error.

## Fixture C — 2048 rerun — UNEXPECTED SEMANTIC DRIFT = 0 ✅

`gabrielecirulli/2048@478b6ec`, attempt `fatt_01M1KPTCSRMWH1GX3G47F14AJ1`.

| | PRIMARY | SHADOW |
|---|---|---|
| content files | 27 | 27 |
| set difference | — | none, either direction |
| media types | — | identical on all 27 |
| blob digests | — | identical on 26 of 27 |
| entry_path / routing / security / schema | — | identical |

Selection dropped exactly the 7 files the existing path drops:
`.gitignore`, `.jshintrc`, `CONTRIBUTING.md`, `README.md`, `Rakefile`,
`style/helpers.scss`, `style/main.scss`.

Remaining differences, both classified:

1. **Bridge assets — EXPECTED VERSIONED DIFFERENCE.** Primary carries the
   legacy single `__ato/replay-bridge-v0.js`; shadow carries the canonical P0
   pair, `__ato/instance-state-bridge-v1.js` and
   `__ato/browser-runner-bridge-v0.1.2.js`. Shadow is the newer contract, and
   the one the Browser Instance State lane requires.
2. **`index.html` blob — EXPECTED VERSIONED DIFFERENCE, consequent on (1).**
   Diffed against the pinned source: the only change is the injected block,
   inserted ahead of the first app script, in the canonical order — the
   parser-blocking state bridge, then the Operation-lane bridge, then
   `js/bind_polyfill.js`. Nothing else in the document moved.

## Fixture A — plain HTML — UNEXPECTED SEMANTIC DRIFT = 0 ✅

`thedoggybrad/2048ontheweb@c71efbb`, primary
`swm_cpl3ago77dwzeptut4zaumt63wbkzvdr7kt6mtyrg3wyug4xumfa` (2026-08-11),
attempt `fatt_01M1KXG55QE7KZ3NPJ3E3V9KK5`. Repository root, `spa_fallback:
false`, 37 files in source.

34 files common, media types identical, blob digests identical except
`index.html`. Differences:

1. **Bridges, and the `index.html` blob** — as Fixture C. Versioned.
2. **`LICENSE.txt`, published by shadow, absent from primary — EXPECTED
   VERSIONED DIFFERENCE, proven rather than assumed.** The existing path's own
   selection rule changed. Two live artifacts of the SAME pinned source,
   `gabrielecirulli/2048@478b6ec`, formed three weeks apart, differ by exactly
   this file:

       swm_ee6elfssjqs3ua...  2026-08-11  27 files  no LICENSE.txt
       swm_iuxlwt2dqf3km...   2026-09-02  28 files  LICENSE.txt present

   `text/plain` was admitted somewhere between those dates. Formation matches
   the September contract; Fixture A's primary predates it. This is the clearest
   evidence yet for the rule you set: the live artifact contract is itself
   versioned, and no single artifact can be treated as the reference.

## Fixture B — Vite / static build — UNEXPECTED SEMANTIC DRIFT = 1 ⛔

`ato-run/ato-e2e-static-spa@1e1be10`, primary
`swm_ct2ae7tvpsox4zieb5sdwclmaodpbetc3uxacdma2oxrvsdehlya`, attempt
`fatt_01M1KXG9J3X21Q1XRCK9T2SM5Y`, `static.output_root=dist`.

    PRIMARY  index.html  assets/index-B5veQxUY.css  assets/index-DAhkYDUq.js
    SHADOW   index.html  app.js

**The existing Static path runs a build; the Formation Static lane does not.**

Neither `assets/index-B5veQxUY.css` nor `assets/index-DAhkYDUq.js` exists at
any path in the pinned source tree. Their names carry Vite's content hashes, so
they were produced by `vite build` and published from the produced `dist/`. The
repository does contain a committed `dist/`, but it holds `dist/app.js` and
`dist/index.html` — stale, pre-build, and what shadow published.

The cause is not a defect to patch: `plan_build` dispatches
`(Lane::StaticWeb, _) => {}`, so a Static intent yields zero build steps by
construction. A Static Compute whose site is generated therefore cannot be
formed by FormationServiceV1 at all — it silently publishes whatever stale
output happens to be committed, which is a worse failure than refusing, because
it succeeds.

This is a missing capability — a Node/npm build lane for Static, the
counterpart of the Python lane — not something to close by widening a table.
Scoping it is a decision for you; it is not in B1-S's remit and will not be
invented here.

## Shadow side effects — 0 across all three fixtures ✅

Across jobs `fjob_01M1KNNCG4B2078VWVJXVRW9XP`,
`fjob_01M1KXFW6WS847ENPYYVSM2XY6` and `fjob_01M1KXFWKC3QZ5CFBGAJV1QM5R`:

    formation_results registered      0
    ComputeSchema created             0   (newest predates every shadow run)
    ComputeInstance created           0
    Run / lease / public URL          0   (unreachable without a result)
    Listing / Post                    0
    capsule_static_web_materializations created since the runs   0

## Static gate — FAIL

Fixtures A and C are clean. Fixture B is blocked on a capability the Static
lane does not have. Static does not go primary, and Python semantic compare
does not start.

---

# Static Build Profile v1 — Fixture B closed

## Why Static needed a build at all

The Static lane assumed `(Lane::StaticWeb, _) => {}` — Static means no build.
Fixture B measured that wrong, and the fixture is built to make the error
visible: `ato-run/ato-e2e-static-spa@1e1be10` carries a checked-in `dist/`
that is a DIFFERENT VERSION of the app from its source.

    source (index.html + src/app.js)   STATIC_FIXTURE_V1
    committed dist/                     STATIC_FIXTURE_V2

The existing path published V1, built. Formation published V2, stale, and
called it success.

## Primary build semantics — recovered from evidence, not inferred

Not "the assets are hashed, so probably Vite". The stored Program Intent
`intent_01KZHVYZ1G4W07TC7GBNA8T0D4` and the existing Builder's own code both
say what happened:

- `origin: inferred`; `build_steps: []`; `build_output_roots: ["dist"]`;
  launch `npm run preview -- --host 0.0.0.0 --port 8000 --strictPort`.
- That launch argv is produced, character for character, by
  `snapshot-builder::authoring_runtime::infer_vite_production_launch`, which
  fires only when `scripts.build` is plainly `… vite build` AND
  `scripts.preview` is plainly `… vite preview`.
- The build itself ran at IMAGE BUILD time —
  `snapshot::rootfs_builder::vite_production_prebuild_cmd` returns
  `"{pm} run build"` keyed on exactly that launch shape, chained after
  `base_image_and_install`. That is why the intent's own `build_steps` is
  empty and a build still happened.
- The static output root came from
  `infer_vite_static_web_outputs` → `vite_out_dir`: the literal `build.outDir`
  in `vite.config.*`, else `dist`, else NOTHING when the override is present
  but unreadable.

For this source that resolves to: package manager `npm` (no lockfile, no
`packageManager`), Node `20.20.2` (nothing declared → the Builder's own
default), `npm install`, `npm run build`, output root `dist`.

## What was added

`StaticBuildProfileV1` — a build profile, not a lane. Node is a build tool
here and nothing else: no Node process, no Node runtime realization, no
`RuntimeLaunchSpec` specialization, no SSR, no server. "Built with Node" is not
"runs on Node", and the ComputeSchema still says browser.

Detector evidence grew the facts the decision needs, all verbatim:
lockfiles (npm/pnpm/yarn/bun + shrinkwrap), `.nvmrc`, `.node-version`,
`volta.node`, `engines.node`, `packageManager`, `scripts.build`,
`scripts.preview`, dependency names, and a three-valued `build.outDir` read —
`Unset` / `Literal` / `Unreadable`, because "no override" and "an override I
cannot read" are different facts.

Decision order: authored `static.build` → detector evidence → fail closed.
Deliberately NOT consulted: whether a `dist/` exists. Fixture B is the
counterexample that makes that rule non-negotiable.

Refusals, each measured against a way to be wrong:

| Situation | Result |
|---|---|
| Two lockfiles naming different managers | `intent_ambiguous_lockfiles` |
| `engines.node` no ladder version satisfies | `intent_unsupported_node` |
| `build.outDir` computed, not literal | `intent_requires_authoring` |
| Compound build script (`vite build && …`) | not built-static |
| `express`/`next`/`nuxt`/… in dependencies | not built-static (it serves itself) |
| `static.build = required`, nothing to infer | `intent_requires_authoring` |

Toolchain and package manager:

- Node ladder `18.20.4 / 20.20.2 / 22.14.0 / 24.18.0`, default `20.20.2` —
  the existing Builder's exact list, so a source resolves to the same Node it
  did there. All four verified present on nodejs.org as `linux-x64` `.tar.gz`.
- Resolution order `.nvmrc` → `.node-version` → `volta.node` →
  `engines.node` (range → ladder) → default. The host's `node` is never the
  answer, for the reason the Python lane already learned the hard way.
- Install mode follows the lock: `npm ci` / `pnpm install --frozen-lockfile` /
  `yarn install --frozen-lockfile`; with no lock, `npm install` and the intent
  records `unpinned-package-manager-resolution` rather than implying a pin.
- `PATH` is the provisioned toolchain's `bin` first: `npm` re-invokes `node`,
  and provisioning one Node while building with another is not a fixed
  toolchain.
- npm/corepack caches point at the cache mount. `HOME` is the workspace, so
  the default would have written `.npm/` into the tree that becomes the
  artifact.

## Network policy — the support matrix, stated

Installing dependencies is dependency resolution, and bubblewrap here cannot
enforce "npm registry only" egress. ADR-018 is not bent for this: the job-level
gate still refuses `publish_enabled` together with
`network: dependency_resolution`.

    Static, no dependency resolution        public untrusted allowed
    Static, dependency-resolving build      trusted / allowlist only
    Python, ANY (dependencies or not)       trusted / allowlist only

The Python row says ANY on purpose, and it is a real constraint rather than a
rounding. The Python lane always provisions its interpreter, and the policy
enum has no class between `denied` and `dependency_resolution`, so even a
stdlib-only program with no dependency graph to resolve must request
`dependency_resolution` and lands in the trusted row. B1 ships with that
limitation stated rather than papered over; lifting it means a third network
class for toolchain provisioning (a fixed pinned URL set, not arbitrary
egress), which is a policy-model change and deliberately not part of B1.

Built-static is NOT enabled for untrusted public sources. Running inside bwrap
is not a claim that a networked npm build is safe.

## Pipeline order, kept

    source → build → output root → selection → extract → instrument → produce

The `&ExtractedStaticWebOutput` parameter still makes the producer unreachable
without extraction. `plan.output_root` for a built-static intent is the build's
output root, so `build::output_root` fails the attempt if the build did not
write it — a build that silently produced nothing cannot reach the producer.

## Fixture B rerun — UNEXPECTED SEMANTIC DRIFT = 0 ✅

Attempt `fatt_01M1KYN0CCZ0KGRDTJK4455VY3`, same pinned source.

| | PRIMARY | SHADOW |
|---|---|---|
| `index.html` | present | present |
| `assets/index-B5veQxUY.css` | present | present, SAME blob digest |
| `assets/index-DAhkYDUq.js` | present | present, SAME blob digest |
| media types | — | identical |
| entry / routing / security | — | identical |

Vite's content hashes are reproduced exactly, so the comparison needed no
"different bundler version" allowance at all. The only differing blob is
`index.html`, and the cause is the bridge injection — primary carries no
bridges for this artifact, shadow carries the canonical pair.

Stale-dist regression, asserted on the real artifact and in a unit fixture:
the served document contains `STATIC_FIXTURE_V1` and references
`/assets/index-*`, NOT `STATIC_FIXTURE_V2` and `/app.js`.

## Browser smoke — PASS

The shadow bundle was served from its manifest (paths, media types and SPA
fallback all resolved through `manifest.files`, never from the blob directory)
at `127.0.0.1`, registered nowhere.

    index load          200, renders "Static fixture v1"
    JS / CSS            200 / 200; built CSS applied (body #10314f)
    app executed        title became "Static fixture v1 — home" (router ran)
    asset 404s          none
    SPA fallback        /deep-link → index.html, title "— deep-link"
    instrumentation     both bridges loaded, state slot present, canonical
                        order: state bridge → runner bridge → app script

One console exception: `invalid_window_identity` from
`browser-runner-bridge-v0.1.2.js`. Harness-induced, not a build defect — the
bridge's own `ato-browser-runner-controller-origins` meta names
`https://ato.run` and `https://stg-app.ato.run`, and this smoke loads the page
top-level on `127.0.0.1`, where there is no controller peer to identify. The
app rendered and routed regardless.

## Static matrix at latest head — all three re-run

| Fixture | content | media | blob diffs | unexpected drift |
|---|---|---|---|---|
| A plain HTML | 34 / 35 | 0 | `index.html` | **0** |
| B Vite built | 3 / 3 | 0 | `index.html` | **0** |
| C 2048 source | 27 / 27 | 0 | `index.html` | **0** |

Classified differences, all versioned: bridge assets (legacy single
`replay-bridge-v0.js` → canonical P0 pair), the `index.html` blob that follows
from them, and Fixture A's `LICENSE.txt` (the existing path admitted
`text/plain` between 2026-08-11 and 2026-09-02 — proven by two live artifacts
of the same pinned source differing by exactly that file).

## Shadow side effects — still 0

Across all four jobs: `formation_results` 0, ComputeSchema 0 (newest still
`12:44:17Z`, before every shadow run), ComputeInstance 0, Run / lease / public
URL 0, Listing / Post 0, new static materializations 0.

## Static gate — PASS

---

# B1-S §8 — Python semantic compare

REFERENCE: the pre-B1 accepted Process definition
`intent_01KZC90YK81BF79BTVAFNPRBN6`, `origin: imported` — an AUTHORED
capsule.toml, not an inference — for
`ato-run/ato-e2e-compute-server@4f442f1ad27ae6a27eb3341283a4bb666cbdda2e`.
A stdlib-only `server.py`: no `requirements.txt`, no `pyproject.toml`, no
`.python-version`, and a per-process nonce in every response so the output
cannot be mistaken for a static capture.

SHADOW: attempt `fatt_01M1KZAB2EN7D75N9915590DDY`, same pinned source, the
capsule.toml's declarations carried across as authored overrides.

## A refusal found first — marker-less Python

The first attempt failed outright:

    no lane matched this source: the Python lane was selected but the source
    carries no Python marker

Not for want of authoring — lane, argv, port and readiness were all declared,
exactly as the capsule.toml declared them. Formation refused for want of a
DEPENDENCY marker. Those are different things, and requiring the second is a
rule that says every Python program must have dependencies. A stdlib program
has none, and the existing path formed this one for months.

Fixed narrowly: when the lane is AUTHORED and the source root holds at least
one `.py` module, the Python lane proceeds with empty Python evidence —
`dependencies: none`, runtime from the policy default. `choose_lane` is
untouched, so auto-detection still requires a marker and a static repository
carrying a helper `build.py` is still a static repository. An authored Python
lane over a source with no module at all still refuses: there is nothing to
run.

## Comparison

| dimension | REFERENCE | SHADOW | verdict |
|---|---|---|---|
| argv | `["python3","server.py"]` | `["python3","server.py"]` | **MATCH** |
| cwd | `"."` | `""` | EXPECTED NON-SEMANTIC — both resolve to the workspace root `/app` |
| endpoint | port 8080 | `http` → 8080 | **MATCH** (`bind = 0.0.0.0` is a host concern, not an intent one) |
| readiness | http, port 8080, path `/` | path `/`, port 8080 | **MATCH** on path and port; `timeout_seconds: 60` is NOT COMPARABLE — intent v1 has no field for it |
| state slots | none | none | **MATCH** |
| bindings | none | none | **MATCH** |
| workspace | source tree | source tree (`capsule.toml`, `README.md`, `server.py`), no `.venv` | **MATCH** |
| runtime | `toolchains: []` — undeclared | `python 3.12.7`, provisioned | EXPECTED VERSIONED DIFFERENCE |
| build steps | `[]` | `[provision-python]` | EXPECTED VERSIONED DIFFERENCE, same cause |

The runtime row deserves its exact reading. This is not "3.11 versus 3.12": the
reference declares NOTHING, and ran whichever `python3` its base image
happened to carry. **Which interpreter actually executed the reference is NOT
COMPARABLE — it was never recorded.** What changed is that the contract now
requires an exact runtime and provisions it, which is the property the Python
lane exists to establish. For this program the two are behaviourally identical
(stdlib `http.server`, no version-sensitive API).

## Process workspace compatibility — PASS

The formed workspace was run on the provisioned interpreter, in place:

    /opt/ato/toolchains/python/3.12.7/bin/python3 server.py
    → HTTP 200
    → <h1>E2E_COMPUTE_SERVER_V1</h1><p>nonce=d5fe43d54ec486f6</p>

A per-process nonce, so this is the dynamic program running, not a captured
response.

## UNEXPECTED SEMANTIC DRIFT — 0

## Two findings that are not drift, and want a decision

**1. A zero-dependency Python program needs the `dependency_resolution`
network class.** `network: denied` refused the build:

    build step "provision-python" needs the network and this job's policy denies it

Provisioning a toolchain IS a network need, but it is not dependency
resolution — nothing about this source's dependency graph is resolved, because
there is no graph. The policy enum has no class between "denied" and
"dependency_resolution", so by the support matrix a stdlib-only Python program
is trusted/allowlist-only. The existing path avoided this by having Python
already in the base image.

The shape of a fix is a third network class (toolchain provisioning: a fixed,
pinned URL set, not arbitrary egress), which would let dependency-free Python
join Static in the public-untrusted row. That is a policy-model change and is
not being made here.

**2. `PYTHONPATH` names a `.venv` that no step creates.** With
`dependencies: none` the intent still sets
`/app/.venv/lib/python3.12/site-packages`, and no `create-site-packages` step
runs. Python ignores a missing `sys.path` entry, so nothing breaks today; it is
recorded because the path is a claim about something that is not there, and a
`.venv` appearing later would be picked up without anything having asked for
it.

## Ready for comparator determinism and rollback drill — yes

---

# B1-S §9 — Comparator determinism

A cutover gate that answers differently on identical input is worse than no
gate: people learn to re-run it until it agrees with them. Two ways this
comparator could have done that were found by reading it, and both are now
closed and pinned.

**Key order.** `note()` compared `JSON.stringify(a)` against
`JSON.stringify(b)`, and `JSON.stringify` preserves key INSERTION order. Two
objects carrying identical facts serialize differently whenever their producers
built them in a different order, so the comparator would have reported drift
that does not exist — intermittently, which is the worst kind. Comparison now
goes through `canonical()`, which sorts object keys at every depth.

**Set-shaped fields.** `runtime_requirements`, `exported_ports`,
`readiness_contracts`, `state_slot_declarations`, `binding_requirements`,
`materializations`, `security.connect_src` and `security.frame_ancestors` are
sets: the same members in a different order mean the same thing. They now
compare through `canonicalSet()`.

Array order is preserved everywhere else, deliberately. `launch.argv` is a
sequence — `["python3","server.py"]` is not `["server.py","python3"]` — and
sorting it would have hidden a real change of program.

Pinned by tests, against the REAL Fixture C manifests:

| test | claim |
|---|---|
| repeated runs | 8 runs of the same input produce one distinct verdict |
| reversed key order | a manifest whose every object has reversed keys — verified to serialize differently — compares equal |
| set vs sequence | reordering `runtime_requirements` and `exported_ports` yields no difference and does not block; reordering `launch.argv` yields exactly `launch.argv` and DOES block |
| unrecorded difference | an extra file is `unexpected_semantic_drift` and blocks, every time |

10 tests in `b1s-shadow-fixture-c.test.ts`, all passing; `tsc --noEmit` clean.

# B1-S §10 — Rollback drill

Run against staging, through the real routes with a real session, on the live
policy. The policy was captured first and restored last.

| step | mode | may_register | reason |
|---|---|---|---|
| 0 baseline | shadow | false | lane static is in shadow |
| 1 static → **primary**, repository NOT allowlisted | **shadow** | false | primary but not allowlisted; running in shadow so the comparison still happens |
| 2 static → primary, repository allowlisted | **primary** | **true** | primary and allowlisted |
| 3 **ROLLBACK** to the captured policy | shadow | false | lane static is in shadow |

Step 1 is the safety property worth having proved: a lane flipped to primary
without an allowlist entry degrades to SHADOW, not to off. The failure mode it
prevents is a half-finished rollout that silently stops comparing.

Rollback was then exercised for real, not just read back: a full Formation
attempt was run on Fixture C after step 3
(`fatt_01M1KZY4T9T5VK5P4J6KTWMEJP`, fence 5, `Accepted`,
`compute_schema_id: ""`). Afterwards:

    formation_results for the job                  0
    newest formation_results row overall           12:44:17Z (pre-shadow)
    newest compute_schemas row                     12:44:17Z (pre-shadow)
    new static materializations since 13:00Z       0
    live artifact swm_iuxlwt2dqf3km… manifest      sha256:fd471ae7…  (unchanged)
    final rollout policy                           static: shadow, python: shadow

The live artifact's digest is byte-for-byte what it was before B1-S began.

# B1-S — COMPLETE

| gate | verdict |
|---|---|
| §1 Static shadow, Fixture C (2048) | PASS — drift 0 |
| §7 Static shadow, Fixture A (plain HTML) | PASS — drift 0 |
| §7 Static shadow, Fixture B (Vite built) | PASS — drift 0 |
| §14 Fixture B browser smoke | PASS |
| §8 Python semantic compare | PASS — drift 0 |
| §3/§6 Shadow side-effect zero | PASS — 0 across 7 attempts |
| §4 Shadow failure isolation | PASS |
| §9 Comparator determinism | PASS |
| §10 Rollback drill | PASS |

**UNEXPECTED SEMANTIC DRIFT: 0**

Four defects were found by this gate and fixed, none of which a digest
comparison would have surfaced:

1. The canonical materializer could not type `ico`/`woff`/`eot` — a live
   artifact it cannot reproduce. Formation would have shipped a site with no
   favicon and fallback fonts, successfully.
2. The Formation Static lane reached the producer without extracting, so its
   bundles carried NO instrumentation. The site would have looked correct and
   silently lost its Browser Instance State. The producer now takes the
   extraction proof type, so the bypass no longer compiles.
3. Static had no build lane. A generated site published whatever stale output
   was committed — a different version of the application, reported as
   success. Closed by Static Build Profile v1.
4. The Python lane refused a stdlib program for want of a DEPENDENCY marker,
   which is not the same as want of authoring.

And one constraint is shipped stated rather than papered over: the Python lane
always provisions its interpreter, and with no network class between `denied`
and `dependency_resolution`, every Python source — dependencies or not — is
trusted/allowlist only.
