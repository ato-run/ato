# Formation 6b-C — 20 → 50 actual coverage measurement

## State

- **measurement designed · preregistered · actual measured · evidence committed**
  (this branch). Merged: not yet. Deployed: **no**. Remote migration: none.
  Model calls: **0**. No new capability implemented.
- #1441 reviewed head `bf4e66d4a52261cd121da800b7eca37330e4609b`, merged
  `c4c285f3e9a751975286ccd78ba49e76e483fa83` (admin; CI failures were base-reproduced
  or a base-reproduced intermittent Linux test, see below).
- Measurement pin `c4c285f3` (Rust identical; checkout `57b063b9` = preregistration
  commit). Classification rules committed before reading results (`5f57437f`).
- Plan: [JSON](formation-coverage-50-plan.json) / [md](formation-coverage-50-plan.md),
  SHA256 `222533282a12c8a3e9694c9fd672e74c07b6e3c24c10a6b931fd4c2d6a56289f`. Raw results SHA256 `bd80e70bf3d448e812259dbea8d12475b44dbf00190bf9e89fb982ff72f6b7c3`.
  Binaries: coverage_baseline `cabca856fe6f2c6a…`,
  worker `ee2c1e402e6111fa…`.
- Host oci-linux-test, linux/aarch64, bwrap+landlock, one local Runtime profile.
  Disk before cleanup 95% (15 GB free) → 56% (129 GB free); only one stale
  (July) cargo target dir was removed. Frozen archives and evidence kept.

## Policy

Existing `coverage_baseline` → `local::run`; Contract inferred from source;
CandidateProducer/DecisionProvider OFF; **network denied**; max 4 attempts;
900 s hard timeout (none hit); no per-app authoring. Success = actual
`fully_satisfied` same-K receipt; K is GET / 200 + source identity only.

## 50/50 terminal results

All 50 reached a typed `formation_result` (0 timeouts, 0 untyped errors).
Letters = progress layers reached (A source · B K · C D · D admitted · E executed ·
F Verifier · G K satisfied · H retained · I functional).

| # | App | Primary | Terminal code | Layers |
|---|---|---|---|---|
| 1 | Excalidraw | known-D/authoring | `preset_node_static_v2_workspace` | A |
| 2 | WBO | known-D/authoring | `preset_node_static_needs_build_script` | A |
| 3 | Uptime Kuma | success | `fully_satisfied` | ABCDEFGH |
| 4 | FreshRSS | known-D/authoring | `preset_node_static_needs_build_script` | A |
| 5 | linkding | effect/policy | `network_denied` | ABC |
| 6 | SearXNG | known-D/authoring | `preset_node_static_needs_lockfile` | A |
| 7 | changedetection.io | known-D/authoring | `preset_no_match` | A |
| 8 | Etherpad | known-D/authoring | `preset_node_static_v2_workspace` | A |
| 9 | Miniflux | known-D/authoring | `preset_no_match` | A |
| 10 | Gitea | known-D/authoring | `preset_node_static_v2_workspace` | A |
| 11 | Vikunja | known-D/authoring | `preset_no_match` | A |
| 12 | Mealie | known-D/authoring | `preset_no_match` | A |
| 13 | HedgeDoc | known-D/authoring | `preset_node_static_v2_workspace` | A |
| 14 | Actual Budget | known-D/authoring | `preset_node_static_v2_workspace` | A |
| 15 | JupyterLab | known-D/authoring | `preset_node_static_v2_workspace` | A |
| 16 | LibreChat | known-D/authoring | `preset_node_static_v2_bun_deferred` | A |
| 17 | ComfyUI | known-D/authoring | `preset_no_match` | A |
| 18 | PdfDing | effect/policy | `network_denied` | ABC |
| 19 | Grist core | known-D/authoring | `preset_node_static_v2_manager_config` | A |
| 20 | File Browser | known-D/authoring | `preset_no_match` | A |
| 21 | 2048 | success | `fully_satisfied` | ABCDEFGH |
| 22 | reveal.js | success | `fully_satisfied` | ABCDEFGH |
| 23 | CyberChef | known-D/authoring | `intent_malformed` | A |
| 24 | IT-Tools | known-D/authoring | `preset_node_static_v2_static_unproven` | A |
| 25 | Squoosh | effect/policy | `network_denied` | ABC |
| 26 | Hoppscotch | known-D/authoring | `preset_node_static_v2_workspace` | A |
| 27 | Wiki.js | known-D/authoring | `preset_node_static_v2_manager_config` | A |
| 28 | Kutt | known-D/authoring | `preset_node_static_needs_build_script` | A |
| 29 | Umami | known-D/authoring | `preset_node_static_v2_workspace` | A |
| 30 | Ghost | known-D/authoring | `preset_node_static_v2_workspace` | A |
| 31 | Radicale | known-D/authoring | `preset_no_match` | A |
| 32 | Calibre-Web | known-D/authoring | `preset_no_match` | A |
| 33 | Shynet | known-D/authoring | `preset_node_static_needs_build_script` | A |
| 34 | text-generation-webui | known-D/authoring | `preset_no_match` | A |
| 35 | PrivateGPT | known-D/authoring | `preset_no_match` | A |
| 36 | Gotify | known-D/authoring | `preset_no_match` | A |
| 37 | Navidrome | known-D/authoring | `preset_no_match` | A |
| 38 | PocketBase | known-D/authoring | `preset_no_match` | A |
| 39 | Glance | known-D/authoring | `preset_no_match` | A |
| 40 | Shiori | known-D/authoring | `preset_node_static_v2_bun_deferred` | A |
| 41 | Vaultwarden | known-D/authoring | `preset_no_match` | A |
| 42 | Lemmy | known-D/authoring | `preset_no_match` | A |
| 43 | Paperless-ngx | known-D/authoring | `preset_no_match` | A |
| 44 | Mastodon | known-D/authoring | `preset_node_static_v2_workspace` | A |
| 45 | Linkwarden | known-D/authoring | `preset_node_static_v2_workspace` | A |
| 46 | NetBox | known-D/authoring | `preset_no_match` | A |
| 47 | BookStack | effect/policy | `network_denied` | ABC |
| 48 | Firefly III | known-D/authoring | `preset_node_static_needs_build_script` | A |
| 49 | OpenRefine | known-D/authoring | `preset_no_match` | A |
| 50 | Jellyfin | known-D/authoring | `preset_no_match` | A |

## Stage funnel

| Layer | Original 20 | New 30 | All 50 |
|---|---|---|---|
| A source recognized | 20 (100%) | 30 (100%) | 50 (100%) |
| B K formed | 3 (15%) | 4 (13%) | 7 (14%) |
| C D available | 3 (15%) | 4 (13%) | 7 (14%) |
| D admitted | 1 (5%) | 2 (7%) | 3 (6%) |
| E executed | 1 (5%) | 2 (7%) | 3 (6%) |
| F verifier ran | 1 (5%) | 2 (7%) | 3 (6%) |
| G K fully satisfied | 1 (5%) | 2 (7%) | 3 (6%) |
| H retained | 1 (5%) | 2 (7%) | 3 (6%) |
| I functional success | 0 (0%) | 0 (0%) | 0 (0%) |

Typed-K PASS 3/50: Uptime Kuma (static-files fallback on a service repo),
2048 and reveal.js (static sites). **None establishes a functional application.**
Uptime Kuma's receipt proves only that its repository tree serves GET / 200.

## Blocker distribution

Primary class — original 20: known-D/authoring 17, effect/policy 2, success 1;
new 30: known-D/authoring 26, success 2, effect/policy 2; all 50: known-D/authoring 43, effect/policy 4, success 3.

Terminal code — original 20: preset_node_static_v2_workspace 6, preset_no_match 6, preset_node_static_needs_build_script 2, network_denied 2, fully_satisfied 1, preset_node_static_needs_lockfile 1, preset_node_static_v2_bun_deferred 1, preset_node_static_v2_manager_config 1.
New 30: preset_no_match 14, preset_node_static_v2_workspace 5, preset_node_static_needs_build_script 3, fully_satisfied 2, network_denied 2, intent_malformed 1, preset_node_static_v2_static_unproven 1, preset_node_static_v2_manager_config 1, preset_node_static_v2_bun_deferred 1.
All 50: preset_no_match 20, preset_node_static_v2_workspace 11, preset_node_static_needs_build_script 5, network_denied 4, fully_satisfied 3, preset_node_static_v2_bun_deferred 2, preset_node_static_v2_manager_config 2, preset_node_static_needs_lockfile 1, intent_malformed 1, preset_node_static_v2_static_unproven 1.

Secondary blockers are **hypotheses** from the preregistered shape and code
facts (runtime catalog = Node 20.20.2/22.14.0, Python 3.11.11/3.12.7/3.13.1),
not measurements: dependency/network 31, runtime/toolchain 21, multi-service 16, Browser/UI 4, effect/policy 1.

By language, the dominant codes read differently from their names:

- `preset_no_match` 20: Python 9, Go 7, Rust 2, Java 1, C# 1 — services with
  no root `package.json`; the known-D frontend has no service/process route.
- node-static refusals 21 (`v2_workspace` 11, `needs_build_script` 5, `bun` 2,
  `manager_config` 2, `lockfile` 1): because a root `package.json` exists, the
  source is routed into the static-build family even when the product is a
  Go/PHP/Python/Ruby service (Gitea, Mastodon, FreshRSS, Firefly III, Shynet,
  SearXNG, Shiori). Most Node workspace hits are services (Ghost, Umami,
  Linkwarden, Etherpad, HedgeDoc), not static frontends.
- `network_denied` 4: linkding, PdfDing (Django), BookStack (PHP) and Squoosh.
  Three of these D are node-static builds of an asset `package.json` inside a
  service repo; if they were admitted, a typed-K pass would not be the app.

## Original 20: regression comparison (6a → current)

12 unchanged (identical D refs where D exists), 7 diagnostic refinements,
**1 regression**:

| App | 6a | Current | Change |
|---|---|---|---|
| Excalidraw | `preset_node_static_needs_lockfile` | `preset_node_static_v2_workspace` | diagnostic refinement |
| Etherpad | `preset_node_static_needs_lockfile` | `preset_node_static_v2_workspace` | diagnostic refinement |
| Gitea | `preset_node_static_needs_lockfile` | `preset_node_static_v2_workspace` | diagnostic refinement |
| HedgeDoc | `preset_node_static_needs_lockfile` | `preset_node_static_v2_workspace` | diagnostic refinement |
| Actual Budget | `preset_node_static_needs_lockfile` | `preset_node_static_v2_workspace` | diagnostic refinement |
| JupyterLab | `preset_node_static_needs_lockfile` | `preset_node_static_v2_workspace` | diagnostic refinement |
| LibreChat | `network_denied` | `preset_node_static_v2_bun_deferred` | regression |
| Grist core | `preset_node_static_needs_lockfile` | `preset_node_static_v2_manager_config` | diagnostic refinement |

**Detected mismatch (LibreChat):** the source has both `package-lock.json`
and `bun.lock`. #1440's known-D frontend routes any Bun lock to node-static/v2,
which defers Bun, so the npm v1 route (6a: K+D formed, then `network_denied`)
is no longer offered. This contradicts the v2 contract ("npm remains v1").
6b-A's eight-app remeasurement did not include LibreChat, so it was not seen
there. Not fixed in this measurement wave.

## Actual incremental effect of 6b-A / 6b-B (original 20, known-D)

| Layer | 6a | after 6b-A | after 6b-B (current) |
|---|---|---|---|
| diagnostic refinement | – | 7 apps | 7 apps (unchanged by 6b-B) |
| K formed (authoring reach) | 4 | 3 (LibreChat lost) | 3 |
| D admitted / executed | 1 | 1 | 1 |
| typed-K verified | 1 | 1 | 1 |
| functional | 0 | 0 | 0 |

6b-B changes nothing in known-D by design; its operation is proposal-only and
published no ID for the six workspace apps (see 6b-B ledger). A better error is
useful but is not coverage: net 6b-A/B effect on original-20 authoring reach
is **−1**.

## CandidateProducer analysis (no calls, no D generated)

`false` 30 (missing runtime catalog, multi-service or network policy — execution
capability, not knowledge), `unknown` 17 (a proposal could name an entrypoint or
workspace, but dependency resolution/service semantics still gate execution),
n/a 3 (successes). No failure is classified as solvable by knowledge alone.

## Next generic gaps (ranked from evidence; not implemented)

1. **Service/process shape at known-D.** 41/47 non-successes have a
   preregistered service/process shape (`preset_no_match` 20, node-static
   refusals on service repos 18, `network_denied` on service repos 3); only 6
   are static frontends (Excalidraw, Actual Budget, Hoppscotch, IT-Tools,
   CyberChef, Squoosh). Highest
   frequency and generality and most likely to yield usable software; highest
   cost, since it needs a service K, launch/port semantics and depends on 2–3.
2. **Dependency acquisition policy.** Every non-static route needs dependency
   resolution (hypothesis 31/50; 4 already stop at `network_denied`, and every
   v2/workspace/Python route would). A policy + provenance decision, not a
   parser; without it no service route can execute under the baseline.
3. **Runtime catalog breadth** (Go 9, PHP 3, Rust 2, Ruby/JVM/.NET 1 each by
   primary language): prerequisite for gap 1 beyond Node/Python.
4. **Static-vs-service classification correctness** when `package.json` is only
   asset tooling (11 non-Node repos: Gitea, Mastodon, FreshRSS, Firefly III,
   Shynet, SearXNG, Shiori, linkding, PdfDing, BookStack, JupyterLab): low cost; prevents misleading typed-K
   "successes" and misattributed node-static refusals.
5. **LibreChat npm+Bun routing mismatch**: small correctness fix restoring v1.
6. Lower frequency/likelihood: pnpm workspace / manager config (static
   frontends only Excalidraw, Actual Budget, Hoppscotch, IT-Tools), Bun (Shiori),
   exact Node from `.nvmrc` "24" (CyberChef), multi-service (downstream of 1).

This ranking is qualitative (frequency × generality × likelihood of usable
software × cost). No next capability was started.

## #1441 CI classification at merge

Rust CI/CLI on `bf4e66d4`: Windows Unix-API compile errors, Ubuntu
`hosted_python_and_node_use_the_common_process_runtime`, macOS hosted-validator
a–d/g1/g2 and `a_process_run_is_owned_by_its_run_until_stopped` — all identical
on main `a6033ecf` run 36525856391/36525856392. Ubuntu
`local_static_server_rejects_browser_state_without_the_run_token`
(ConnectionReset): intermittent on Linux for both head (1/3 pass) and base
(4/5 pass); macOS 3/3 pass; #1441 does not touch portable-application.
No Formation-specific failure.

## Reproduction

`scripts/acceptance/coverage/coverage-50.py --checkout <57b063b9> --old-archives
<6a sources> --new-archives <new sources> --bin <coverage_baseline + worker>
--output <dir>`; classification `classify-50.py`. Archives are fetched from the
plan's `archive_url` and must match `archive_sha256`. Per-app observation
hashes, attempts, typed failures, receipts and retained refs are in
[JSON](formation-coverage-50.json). The 6a ledger is not modified.
