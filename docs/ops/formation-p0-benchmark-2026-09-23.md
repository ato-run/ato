# P0 20 Apps Formation Benchmark — baseline (2026-09-23)

Machine-readable: `formation-p0-benchmark-2026-09-23.json` (same directory).

## Result in one paragraph

**0 of 20 apps reached any Level.** None produced a SatisfyRequest, so the Coordinator saw no candidate, attempt or route: every app was refused by the requester before submission. The first blocker was source size for 12 apps (Runtime Network inline limit 32 MiB), an `exec` build step in the D for 4, a symlink in the source tree for 3, and a second serving process for 1. Independently of the source, **only 1 of the 20 upstream-documented Ds (searxng) can be projected onto today's execution plan**. The authoring grammar can write every one of them — including preparation steps (`ato.process@1` `op = "exec"`) — but the current Derivation projection cannot project an authored `exec` step into an EffectiveBuildPlan (16 Ds), projects exactly one serving step (2), and provisions only a Python runtime (1). A labeled counterfactual on searxng (one sample-config symlink removed from a copy) reached **Level 3 on x86_64** and then stopped on two further blockers: an Ato build executor defect (undrained stdout pipe hangs dependency installs on ARM64) and the browser agent model refusing the page.

## Setup

- Ato `973d71e2 (main, includes #1375 sibling browser sandbox — the pre-benchmark x86 fix)`; ato-api `25d59663 (main)`; ato-api under `wrangler dev --local`, local D1/R2 (migration 0288 applied locally only).
- `rt_sugamo-x86` — linux/x86_64, containment bwrap+landlock, browser: contained, Chrome for Testing 145, chrome-no-sandbox (AppArmor userns restriction).
- `rt_oci-arm64` — linux/aarch64, containment bwrap+landlock, browser: contained, Chromium 149, chrome-sandbox.
- `rt_mac-m1` — macos/aarch64, containment none, browser: none.
- **source**: shallow clone of the default branch head; .git excluded by the snapshot; LFS not smudged (recorded); submodules not initialised (recorded)
- **d**: one hand-written route per app from upstream documentation; no source patches; overlay via `--route`
- **k**: K0 = source identity + documented health path (if any) + GET / → 200; K1 = one small Browser Contract prompt per process route
- **runtime_network**: `ato form --runtime-network --mode all` against three owned Runtimes
- **retry**: first honest attempt for all 20; no D description errors were found, so no D retry. searxng got one diagnostic counterfactual (not counted)
- **d_gate_probe**: each D submitted once against an intentionally empty Initial Condition to see whether Ato's D grammar/projection accepts it at all — a probe of D, not of the app; not counted

### Before the baseline: x86_64 browser smoke (not counted)

The contained Browser Verifier failed on the Ubuntu 26.04 x86_64 Runtime (`browser_failed: connect ECONNREFUSED`): its nested bubblewrap cannot create a user namespace under AppArmor's unprivileged-userns restriction, and the preflight had not detected that. Fixed before the baseline (ato#1375: the browser runs in a sibling sandbox started by the worker). After the fix: x86_64 `runtime.browser=true`, `verifier.browser.containment=bwrap`, Browser PASS → VerifiedRoute (Chrome 145, `chrome-no-sandbox`); ARM64 Browser PASS (Chrome 149, `chrome-sandbox`); exfiltration fixture on x86_64 → `boundary_violation`, every off-origin request blocked.

## Per app

| # | App | SHA | Source | Lang | D (documented path) | Level | Primary failure | D-gate probe | Secondary gaps | Time |
|---|---|---|---|---|---|---|---|---|---|---|
| 1 | open-webui/open-webui | `8bd8b4fac5e0` | 41.6 MiB / 5065 files | Python | normal-build+process — single-process (+ optional external model service) | — | **source_too_large** | exec step | external_service_required, persistent_state, toolchain_missing, unsupported_build_step | 0.53 s |
| 2 | louislam/uptime-kuma | `e62702d868a0` | 8.2 MiB / 808 files | JavaScript | normal-build+process — single-process | — | **unsupported_build_step** | exec step | egress_required, persistent_state, runtime_capability | 1.6 s |
| 3 | go-gitea/gitea | `816413034983` | 32.4 MiB / 6326 files | Go | normal-build+process — single-process (SQLite) | — | **source_too_large** | exec step | persistent_state, toolchain_missing, unsupported_build_step | 0.69 s |
| 4 | usememos/memos | `05a2c6db7a3e` | 11.5 MiB / 1307 files | Go | normal-build+process — single-process (SQLite) | — | **unsupported_build_step** | exec step | persistent_state, toolchain_missing | 2.2 s |
| 5 | Stirling-Tools/Stirling-PDF | `455ad987181d` | 218.2 MiB / 8271 files | TypeScript | normal-build+process — single-process (+ native tools: LibreOffice, Tesseract, qpdf) | — | **source_too_large** | non-Python runtime | native_dependency, runtime_capability, source_layout | 1.16 s |
| 6 | excalidraw/excalidraw | `2b9da9610083` | 53.6 MiB / 1291 files | TypeScript | static-build — static | — | **source_too_large** | exec step | browser_static_lane_unsupported, unsupported_build_step | 0.2 s |
| 7 | nocodb/nocodb | `9901007c1b30` | 151.3 MiB / 4756 files | TypeScript | normal-build+process — single-process (SQLite default) | — | **source_too_large** | exec step | persistent_state, runtime_capability, unsupported_build_step | 0.6 s |
| 8 | metabase/metabase | `6ec07f75184d` | 380.4 MiB / 22331 files | Clojure | normal-build+process — single-process (H2 default) | — | **source_too_large** | exec step | build_resource_limit, runtime_capability, toolchain_missing, unsupported_build_step | 2.82 s |
| 9 | n8n-io/n8n | `67fadbc64b22` | 192.6 MiB / 29304 files | TypeScript | normal-build+process — single-process (SQLite default) | — | **source_too_large** | exec step | persistent_state, runtime_capability, source_layout, unsupported_build_step | 3.27 s |
| 10 | node-red/node-red | `1e85f1efbc88` | 33.7 MiB / 1532 files | JavaScript | normal-build+process — single-process | — | **source_too_large** | exec step | persistent_state, runtime_capability, unsupported_build_step | 0.27 s |
| 11 | grafana/grafana | `533b5986711c` | 205.9 MiB / 23416 files | TypeScript | normal-build+process — single-process (SQLite default) | — | **source_too_large** | exec step | build_resource_limit, source_layout, toolchain_missing, unsupported_build_step | 2.58 s |
| 12 | immich-app/immich | `959d46c6547e` | 105.1 MiB / 3476 files · 1 submodule | TypeScript | documented-multi-service — multi-service (server, machine-learning, PostgreSQL + pgvecto, Redis/Valkey) | — | **source_too_large** | 2 serving steps | database_service, gpu, multi_service, queue_service, runtime_capability, source_layout, submodule | 0.44 s |
| 13 | navidrome/navidrome | `961ee8c4136f` | 18.4 MiB / 2180 files | Go | normal-build+process — single-process (+ taglib, optional ffmpeg) | — | **source_layout** | exec step | native_dependency, persistent_state, toolchain_missing, unsupported_build_step | 2.27 s |
| 14 | FreshRSS/FreshRSS | `64f7f24c6e2c` | 10.8 MiB / 1277 files | PHP | documented-multi-service — web server + PHP-FPM (two processes) | — | **multi_service** | 2 serving steps | persistent_state, reverse_proxy, runtime_capability | 2.12 s |
| 15 | searxng/searxng | `2ed96e6fcfc9` | 19.7 MiB / 1003 files | Python | direct-process — single-process | — | **source_layout** | accepted (source-dependent lane check only) | browser_inconclusive, build_failed | 2.15 s |
| 16 | paperless-ngx/paperless-ngx | `b11f1f845904` | 88.5 MiB / 1552 files | Python | documented-multi-service — multi-service (webserver + consumer/worker + Redis broker) | — | **source_too_large** | exec step | migration, multi_service, persistent_state, queue_service, unsupported_build_step | 0.28 s |
| 17 | coder/code-server | `8a7bf87a4d66` | 1.7 MiB / 257 files · LFS · 1 submodule | TypeScript | normal-build+process — single-process | — | **source_layout** | exec step | git_lfs, runtime_capability, source_layout, submodule, unsupported_build_step | 0.23 s |
| 18 | portainer/portainer | `d661cbc0bbe2` | 18.6 MiB / 5345 files | TypeScript | normal-build+process — single-process (manages a Docker/Kubernetes endpoint) | — | **unsupported_build_step** | exec step | docker_socket, persistent_state, toolchain_missing | 4.98 s |
| 19 | gethomepage/homepage | `d1c1ee7a1ef0` | 10.6 MiB / 1500 files | JavaScript | normal-build+process — single-process | — | **unsupported_build_step** | exec step | runtime_capability, toolchain_missing | 2.29 s |
| 20 | mickael-kerjean/filestash | `dd80cc78ca33` | 33.2 MiB / 1530 files | Go | normal-build+process — single-process | — | **source_too_large** | exec step | toolchain_missing, unsupported_build_step | 0.21 s |

Level `—` = not even Level 0 (planning never completed). All times are the requester's wall clock to refusal (source snapshot and freeze or planning); no build, realization or verification ran in the baseline.

## Aggregate

- Level 5 (VerifiedRoute): **0/20** · Level 4+: 0 · Level 3+: 0 · build success: 0 · planning success: 0 · reached the Coordinator: 0

Primary failure (first blocker observed):

| Primary failure | Apps |
|---|---|
| source_too_large | 12 |
| unsupported_build_step | 4 |
| source_layout | 3 |
| multi_service | 1 |

Every gap an app has (primary or secondary; observed, D-gate probe or analysis — see the JSON for each tag):

| Gap | Apps |
|---|---|
| unsupported_build_step | 16 |
| source_too_large | 12 |
| persistent_state | 11 |
| runtime_capability | 10 |
| toolchain_missing | 9 |
| source_layout | 7 |
| multi_service | 3 |
| native_dependency | 2 |
| build_resource_limit | 2 |
| queue_service | 2 |
| submodule | 2 |
| external_service_required | 1 |
| egress_required | 1 |
| browser_static_lane_unsupported | 1 |
| gpu | 1 |
| database_service | 1 |
| reverse_proxy | 1 |
| build_failed | 1 |
| browser_inconclusive | 1 |
| migration | 1 |
| git_lfs | 1 |
| docker_socket | 1 |

D-gate probe (each D alone, against an empty Initial Condition): every D parses under the authoring grammar; the Derivation projection accepted 1 (searxng) and refused 16 because it cannot project an authored `exec` step into an EffectiveBuildPlan, 2 because it projects exactly one serving step, 1 because it provisions only Python.

## searxng counterfactual (not counted)

Change: one symlink removed from a copy of the source (utils/templates/etc/apache2 → httpd, a sample Apache config; no executed code). x86_64: dependency install, realization and typed K (/healthz, /, source identity) PASS (Level 3); Browser K inconclusive twice — the agent model refused the page ('Content Exists Risk'). ARM64: build failed after the 15-min step budget; the dependency install was observed blocked writing to an undrained stdout pipe. macOS: filtered (no process containment, no browser verifier).

- satisfy `01M36CFXFS3AT1XPH0D38EAAPR` → **unsatisfied**, effective K `sha256:68c30c286b3d…`
  - candidate `rt_oci-arm64` admissible=True rank=1 reasons=[]
  - candidate `rt_sugamo-x86` admissible=True rank=2 reasons=[]
  - candidate `rt_mac-m1` admissible=False rank=None reasons=['requirement_unmet', 'requirement_unmet', 'requirement_unmet', 'verifier_unavailable']
  - attempt `rt_oci-arm64` → fail (formation_failed) · HTTP K [] · browser None · 2026-09-23T05:40:52.302Z → 2026-09-23T05:57:02.141Z
  - attempt `rt_sugamo-x86` → inconclusive (browser_contract_inconclusive) · HTTP K [('http-0', 'satisfied'), ('http-1', 'satisfied'), ('source-identity', 'satisfied')] · browser inconclusive · 2026-09-23T05:57:04.120Z → 2026-09-23T05:58:09.258Z
- satisfy `01M36DGZ1YQ6V3B6NP6QG5MVDJ` → **unsatisfied**, effective K `sha256:68c30c286b3d…`
  - candidate `rt_sugamo-x86` admissible=True rank=1 reasons=[]
  - candidate `rt_mac-m1` admissible=False rank=None reasons=['exact_runtime_mismatch', 'requirement_unmet', 'requirement_unmet', 'requirement_unmet', 'verifier_unavailable']
  - candidate `rt_oci-arm64` admissible=False rank=None reasons=['exact_runtime_mismatch']
  - attempt `rt_sugamo-x86` → inconclusive (browser_contract_inconclusive) · HTTP K [('http-0', 'satisfied'), ('http-1', 'satisfied'), ('source-identity', 'satisfied')] · browser inconclusive · 2026-09-23T05:58:55.111Z → 2026-09-23T05:59:49.048Z

## Defects found (not fixed during the baseline)

- **apps/formation-worker/src/build.rs run_step** — build step stdout/stderr are piped but not read until exit; output beyond one pipe buffer blocks the step until its time budget (15 min). Evidence: searxng ARM64 attempt: pip blocked in anon_pipe_write for >9 min; attempt formation_failed after 16 min. Impact: blocks the Python lane for any non-trivial dependency set.
- **source resolver (RESOLVER_CONTRACT_V1)** — the resolver contract defines no symlink entry, so any symlink refuses the whole source; supporting it is a resolver semantics change, not a parser fix. Evidence: searxng, navidrome, code-server refused; 4 more repos contain symlinks. Impact: 7/20 repos.

## What to build next: missing primitives

`blocked` = apps for which this primitive is necessary (observed or analysis). **No single primitive unlocks any app on its own** — every app has at least two independent blockers — so the unlock column is given per bundle below.

| Primitive | Layer | Scope | Blocked apps |
|---|---|---|---|
| Projection of authored preparation/build steps (ato.process@1 op=exec) into an EffectiveBuildPlan — the grammar already expresses them | Derivation projection + build executor | large | 16: code-server, excalidraw, homepage, gitea, grafana, uptime-kuma, metabase, filestash, n8n, navidrome, nocodb, node-red, open-webui, paperless-ngx, portainer, memos |
| Source transport beyond the 32 MiB inline SatisfyRequest (content-addressed upload) | Runtime Network protocol + Coordinator storage | medium | 12: Stirling-PDF, excalidraw, gitea, grafana, immich, metabase, filestash, n8n, nocodb, node-red, open-webui, paperless-ngx |
| Go / pnpm / yarn build toolchains inside the build sandbox | toolchain provisioning | medium | 9: excalidraw, homepage, gitea, grafana, filestash, navidrome, open-webui, portainer, memos |
| Node as a process runtime (not only a build tool) | Derivation projection + runtime provisioning | medium | 7: code-server, homepage, immich, uptime-kuma, n8n, nocodb, node-red |
| Contained symlinks in the source tree — a source resolver semantics extension (new resolver contract version, tree identity includes link targets), not a parser fix | source resolver / Initial Condition identity | medium | 7: Stirling-PDF, code-server, grafana, immich, n8n, navidrome, searxng |
| Multi-service topology (several serving processes, service dependencies) | Derivation + realization | large | 3: FreshRSS, immich, paperless-ngx |
| Java / PHP runtimes | runtime provisioning | medium | 3: FreshRSS, Stirling-PDF, metabase |
| Database / queue service bindings (PostgreSQL, Redis) | bindings | large | 2: immich, paperless-ngx |
| Submodules / Git LFS in the Initial Condition | source acquisition | small | 2: code-server, immich |
| Drain build-step output (fix: run_step leaves stdout/stderr pipes unread; >64 KiB output hangs to the step budget) | formation-worker build executor (defect) | small | 1: searxng — observed on searxng ARM64; affects every D whose dependency install or build prints more than a pipe buffer — i.e. effectively every app once builds exist |
| Browser agent model fallback when the provider refuses content | Browser Verifier | small | 1: searxng — observed twice on searxng x86_64 (counterfactual) |
| Authored static build + static-lane browser realization | Derivation + static lane | medium | 1: excalidraw |
| Docker socket binding | bindings (security-sensitive) | large | 1: portainer — UI may load without it; management does not |

Cumulative bundles (analysis unless stated):

| Bundle | Apps plausibly reaching Level 5 | Confidence | Basis |
|---|---|---|---|
| B1: symlinks + build-output drain (+ agent model fallback) | 1: searxng | high | counterfactual: x86_64 reached Level 3; the two remaining observed blockers are the ARM64 pipe hang and the model refusal |
| B2: B1 + build steps + Node process runtime + pnpm/yarn toolchains | 3: searxng, uptime-kuma, homepage | medium | analysis: sources ≤ 32 MiB, no symlink, single process; uptime-kuma needs GitHub egress during build |
| B3: B2 + Go toolchain | 6: searxng, uptime-kuma, homepage, memos, portainer, navidrome | low-medium | analysis; navidrome also needs taglib, portainer is only a UI without a Docker socket |
| B4: B3 + source transport > 32 MiB | 14: searxng, uptime-kuma, homepage, memos, portainer, navidrome, gitea, filestash, node-red, open-webui, nocodb, n8n, grafana, excalidraw | low | analysis; grafana/metabase builds are memory-heavy; excalidraw additionally needs an authored static build and has no Browser K |
| Not unlocked by B4 | — | medium | still blocked: immich-app/immich (multi-service + PostgreSQL + Redis); paperless-ngx/paperless-ngx (multi-service + Redis); FreshRSS/FreshRSS (web server + PHP); Stirling-Tools/Stirling-PDF (Java + native tools); metabase/metabase (Java/Clojure); coder/code-server (submodule + LFS + heavy build) |

Two of these are not what their size suggests. **Build steps**: the authoring grammar already expresses `exec`; what is missing is its projection into an EffectiveBuildPlan and its execution. **Symlinks**: not a parser fix — accepting them changes the source resolver's semantics (which entries a source tree may contain and how they enter the tree identity), so it needs a new resolver contract version rather than dropping a check.

Reading: the smallest step with measured evidence is B1 (two small fixes + a model fallback → searxng). The largest single lever is **build/preparation steps in an authored D** (16/20 need it), but it only pays off together with non-Python toolchains/runtimes (Node for 7, Go for 6) and, for 12 apps, a source transport beyond 32 MiB. Multi-service and service bindings (3 apps) come last.

## Not measured

- Any build, realization or verification of the 20 apps as pinned (none got past the requester).
- Per-stage timings beyond the requester (none ran); the searxng counterfactual gives one data point: x86_64 attempt 55–65 s end to end (dependency install + realization + typed K + browser), ARM64 16 min to the build budget.
- Behaviour of persistent state, migrations, egress and sockets at run time — listed as analysis gaps only.
