# Formation P0 remeasure on the foundation stack — 2026-09-26

A foundation regression check after Stage 4, not coverage work: no language,
runtime family or multi-service support was added for it.

## Method

- Stack: Runtime/requester built from ato `6a885970` (#1408 Stage 4 over #1407/#1406; later stack commits are docs/scripts only),
  ato-api `b0236692` (#693 over #692); local Stage 4 Coordinator (Miniflare
  D1/R2, migrations through 0300), native Linux aarch64 Runtime with the
  toolchains node 20.20.2/22.14.0, python 3.12.7, pnpm 9.15.4, yarn 1.22.22.
- The same 20 pinned commits and the same hand-written routes as the
  2026-09-23 benchmark (`formation-p0-benchmark-2026-09-23.json`); shallow fetch
  of the pinned commit, `.git` removed, no submodules/LFS; one first honest
  attempt, typed K0 only. The Browser prompt (K1) was **not run** for any app
  (no contained Browser judge on this host; the Chrome baseline gate is open).
- Harness: `scripts/acceptance/formation-p0-remeasure.py`. Levels as before:
  0 source/planning reached the Runtime, 1 build, 2 surface up, 3 typed K,
  4 Browser K (not run here), 5 VerifiedRoute stored.
- Five apps (code-server, excalidraw, n8n, node-red, searxng) were first refused
  by the Coordinator with `source_upload_quota`: the acceptance owner had used
  its 16 source objects in earlier acceptance runs, and 0298 deliberately does
  not recycle them (a known operational follow-up). They were rerun under a
  second acceptance owner; the quota refusals are not counted as app results.

## Result by foundation layer (first blocker)

| layer | apps |
|---|---|
| VerifiedRoute (typed K PASS + retained replay PASS) | 1 |
| source transport | 1 |
| D authoring/detection | 2 |
| execution lowering | 3 |
| runtime capability | 7 |
| build/toolchain | 6 |
| K verification / Browser judgment / retained replay failure / UNKNOWN / publication | 0 / not run / 0 / 0 / 0 |

| app | layer | level | first blocker |
|---|---|---|---|
| searxng/searxng | verified_route | 5 | typed K PASS, VerifiedRoute (attempt `01M3D7Z4DZJ597B6TE4TNKVVK1`), retained replay satisfied |
| metabase/metabase | source_transport | — | Error: cannot add source/locales/sr.po to the source archive  Caused by:     source object exceeds archive cap |
| gethomepage/homepage | d_authoring_detection | — | Error: package_manager_version_unresolved: the pnpm version cannot be resolved exactly: the source carries a pnpm lockfile and package.json declares no packageManager; de |
| nocodb/nocodb | d_authoring_detection | — | Error: package_manager_version_unresolved: the pnpm version cannot be resolved exactly: the source carries a pnpm lockfile and package.json declares no packageManager; de |
| FreshRSS/FreshRSS | execution_lowering | — | Error: projection_serving_steps: a route this build can execute has exactly one serving step; this one has 2 |
| immich-app/immich | execution_lowering | — | Error: projection_serving_steps: a route this build can execute has exactly one serving step; this one has 2 |
| paperless-ngx/paperless-ngx | execution_lowering | — | Error: projection_serving_steps: a route this build can execute has exactly one serving step; this one has 2 |
| mickael-kerjean/filestash | runtime_capability | — | Error: intent_unsupported_runtime: runtime go 1.25 is not one this build provisions exactly (none) |
| go-gitea/gitea | runtime_capability | — | Error: intent_unsupported_runtime: runtime go 1.25 is not one this build provisions exactly (none) |
| grafana/grafana | runtime_capability | — | Error: intent_unsupported_runtime: runtime go 1.25 is not one this build provisions exactly (none) |
| usememos/memos | runtime_capability | — | Error: intent_unsupported_runtime: runtime go 1.25 is not one this build provisions exactly (none) |
| navidrome/navidrome | runtime_capability | — | Error: intent_unsupported_runtime: runtime go 1.25 is not one this build provisions exactly (none) |
| portainer/portainer | runtime_capability | — | Error: intent_unsupported_runtime: runtime go 1.25 is not one this build provisions exactly (none) |
| Stirling-Tools/Stirling-PDF | runtime_capability | — | Error: intent_unsupported_runtime: runtime java 21 is not one this build provisions exactly (none) |
| coder/code-server | build_toolchain | 0 | the candidate could not be formed from this source |
| excalidraw/excalidraw | build_toolchain | 0 | the candidate could not be formed from this source |
| n8n-io/n8n | build_toolchain | 0 | the candidate could not be formed from this source |
| node-red/node-red | build_toolchain | 0 | the candidate could not be formed from this source |
| open-webui/open-webui | build_toolchain | 0 | the candidate could not be formed from this source |
| louislam/uptime-kuma | build_toolchain | 0 | the candidate could not be formed from this source |

## Reading

- Compared with 2026-09-23 (0/20 reached the Coordinator; 12 blocked by the
  32 MiB inline source limit), source transport now blocks only metabase
  (380 MiB > the 256 MiB object cap). Seven apps reached a Runtime attempt and
  one (searxng) reached a VerifiedRoute whose retained candidate replayed with
  a fresh receipt.
- Runtime capability (Go 1.25, Java 21 not provisioned) and multi-service
  routes are coverage work outside this track. The two pnpm routes need an
  exact pnpm pin in D (authoring), not a Runtime change.
- Build failures happened inside dependency installation/build steps
  (npm "Exit handler never called", engine warnings followed by exit 1, a
  release download, `yarn run build` exit 127, n8n's install exceeding its
  step time budget). None is a K, receipt, UNKNOWN or publication defect.
- No foundation regression was found: every classified blocker is upstream of
  or outside the common attempt/verification/receipt path.
