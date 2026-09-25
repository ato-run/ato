# Formation foundation track — working verification ledger

This is an implementation ledger, not a completion declaration. No deployment,
remote migrations, flags, workflows or manual CI reruns are part of this track.

## 2f / PR #1406

Base `de4e4fc3645abfc5d2f6d0f62f2bfc0a95d9427b`.
Head `ab667b82c3f04e40e66598f8e4a2d0d9ca28b02a`.
Implemented and locally/integration verified; unmerged and undeployed.

The actual Coordinator was isolated Miniflare with D1/R2 and the API code at
`e8b0056b48bdb8c3bf0f578edfa610aa0faf0b43` (tree-identical to #691's reviewed
head). The Runtime and requester were native Linux arm64 Rust executables built
from the 2f source. All rows below were `satisfied`, with one accepted route,
`pass`, and a fresh receipt `fully_satisfied=true`:

| case | attempt | K | D |
|---|---|---|---|
| Static | `01M3BZPV0P4EYHMQ949BAXD17Z` | `sha256:480f2e7a190c1047f19447d80416e3582b4a2943a90b9bb8e6a067736145fec6` | `sha256:b4feb08991385b254dd09600a258f27ee7b09b375037db099d8176a4de515d7c` |
| Static 64MiB source | `01M3BZQK9F1WQN83S4ST1WX8YY` | `sha256:03c2fdd7263872afce19987e9148425bccb47b37b8c63ef09ee44995a0e63f9a` | `sha256:468e096962874c33564b789edc86abbac33252ba2d075b39d72cc1fa0c5427d4` |
| Python | `01M3BZYGSGZQY4MKF7C5CT61KQ` | `sha256:22d8402ee34500e845830856d2739abbc7b09bd980d4d2aa40937f96debf65df` | `sha256:3a4c35b63ea012300473b137c0d877a44d7aa27e845a8a17d32ab33ac2f755a7` |
| Node | `01M3BZYPVTFH17JTEBM96FHKRC` | `sha256:ddfa239c2ec8a77ed8b9cb04475562a4be6e10ff563d883cb7edb550e830437d` | `sha256:1d05c0b7bf67e43e343430007330c20e50b1b53cd4d3d9eb4fb96877f15aafdd` |

Process requests with default `network=denied` first produced typed
`network_denied`, `not_started`, no receipt or route. The successful requests
explicitly authorized dependency resolution; no deployment flag was changed.
Harness setup failures (missing destination directory, Node18 instead of Node22,
x86 binary on arm64, incorrect test-token prefix) preceded these successful runs
and are not code-test passes. Request JSON/runtime logs are kept in the task's
`.tmp/2f-actual` and isolated host acceptance directories.

## 3d in progress

Stack base is exactly the #1406 head above, not a merged main.
Rust descriptor and common-launch replay implementation are in progress.
Linux `retained_replay` initially passed all three Static/Python/Node cases:
source Formation, artifact capture, source/build workspace deletion, fresh attempt
on a different runtime identity, same K/D and fresh PASS receipt. These are local
artifact tests, **not durable Coordinator replay acceptance**. The macOS run
only actually executes Static; process cases skip without containment.

Durable object publication/authorization and typed ticket transport are still
being connected. Stage 4, restart acceptance, final P0 remeasurement and final
documentation cleanup are not complete. No foundation completion claim is made.

### 3d durable acceptance (subsequent evidence)

The preceding local-only description records the earlier checkpoint. Subsequently,
actual isolated Miniflare D1/R2 Coordinator and native Linux arm64 Rust Runtime
completed the durable path. The Coordinator was restarted, all three test source
objects were deleted, and fresh Runtime work/out directories were used. Owner
requester acceptance called the common Rust receipt authority. Each case retained
same K/D, new attempt/receipt, positive transfer/extraction, and stored charge 0.

| case | source attempt | fresh replay attempt | retained ref |
|---|---|---|---|
| Static | `01M3C1GB9XWZMMXG4PS1KP287P` | `01M3C1SXT997HK7KFE0ZTRV1GY` | `sha256:de069a1cede5eaed44ca9045775de8b0adca03304dc564f64f9062ca8e94a8b9` |
| Python | `01M3C1GHE1NY9Q9W8VZF498QRP` | `01M3C1T0EGRWAKBXPDA3XP2XWD` | `sha256:39724eaa407f3c08b080ce78d3e39dfe52a2681586b941e4775264b597480a96` |
| Node | `01M3C1GQK3603BBKJ28Q7QD0RE` | `01M3C1T34YRMSQQHX3TB4SHC9Y` | `sha256:22460097456b1e7ea0dedc80d483a785e3747b147846ec46d21620e3b8680104` |

Source removal marker: `removed=3`, `2026-09-25T10:28:24.932Z`. Logs retained in
`.tmp/3d-actual/replay` and the isolated host `formation-foundation/3d` directory.
This is real HTTP/execution/storage acceptance, not Browser full E2E. Browser
baseline limitations remain open; no fresh Chrome success is claimed.

Worker regression: 30 Runtime Network tests passed, one explicitly ignored
harness test; typed ticket roundtrip separately passed. Native authority 7 passed.
API Coordinator 67, source object 6, historical wire 11 passed, including new
retained ownership/fence, immutable ready, concurrent finalize, corrupt/missing
artifact, old receipt and unfinished execution refusals. Sandbox socket denial
on an earlier test invocation is not a product failure; the permitted run passed.

Stage 4, final stack regressions/P0 and final docs cleanup remain pending.
All of this is implemented/local integration evidence, not merged or deployed.

## Stage 4 — durable deterministic SearchState

Stack: ato `feat/formation-durable-search-state` on #1407; ato-api
`feat/formation-durable-search-state` on api#692 (migration 0300, local only).
Design: [ADR-034](../rfcs/draft/ADR-034-formation-search-state.md).
Implemented and locally/integration verified; unmerged and undeployed.

### Actual acceptance (real Coordinator restarts, real Rust Runtime)

Coordinator: isolated Miniflare D1/R2 with the production Runtime Network
routes, ato-api `31b60f0a`, receipt/search authority WASM from ato `f0336685`
(sha256 `75fb1f2e…`), continuing the 3d Coordinator's persisted state with
migration 0300 applied on top. It was restarted as a separate process at each
point below. Runtime and requester: native Linux aarch64 (`oci-linux-test`)
built from the Stage 4 tree. Harness: `scripts/acceptance/formation-stage4-search.py`.
Fixture: the Python `notes` source with two routes over one K — D1 serves the
tree with `http.server` (no `/health` → typed K failure), D2 is the app; a
third, `non-repeatable`, for the effect case. Fault injection is limited to SQL
triggers that hold exactly one insert/claim, a read-only Runtime attempt journal
(history unavailable → UNKNOWN), and a wrong requester effect hint.

| case | result | evidence |
|---|---|---|
| 1 D1 fail → restart → D2 PASS | PASS | D1 `01M3C65BHYQRX4HGW82H0CXYV4` fail `http_status_mismatch` (record finished); restart between D1 failure and D2 issue kept 1 attempt, `attempts_used=1`; D2 `01M3C65K3KJGCR86SJ8JJKXNBA` pass, fresh receipt `fully_satisfied=true`, K `sha256:ce1e9c67…`, requester `accept_verified_routes` accepted; restart after PASS: 1 route, 2 attempts, no second charge; frozen identity length unchanged |
| 2 D1 UNKNOWN → restart → no D2 → resolution → D2 PASS | PASS | D1 `01M3C66P6Y5ATAH616X2PRENNQ` UNKNOWN (`history_unavailable`); across restart and 20 s of a healthy Runtime polling, no D2 and no termination reason; owner resolution citing the terminated Runtime; restart; D2 `01M3C67CRPGRDNC56D15DZ27TF` pass; D1 kept UNKNOWN+resolved, `attempts_used=2`, D1's reservation not refunded |
| 3 retained replay without sources, route usable after restart | PASS | source attempt `01M3C67G2DTD036KX44GTR8S7M` retained `sha256:11037562…`; 6 source objects deleted; restart; replays `01M3C67MPVR84XTADQ2A6XW3X1` and `01M3C67RPSEZGZ7S7RKH050DAK` (fresh attempts, same K/D, stored 0), each followed by a restart after which the route is still listed |
| 4 budget exhaustion | PASS | `max_attempts=1`, D1 fail, D2 untried → `exhausted` / `budget_exhausted` |
| 5 every D fails | PASS | restart directly after claim (result delivered to the restarted Coordinator) → both D `fail` → `candidates_exhausted` |
| 6 effect uncertainty | PASS | Runtime attested `non-repeatable`, refused `effect_policy`, record `not_started` → `effect_unknown`; D2 never issued |
| 7 concurrent advance | PASS | 3 Runtime workers + 8 parallel pollers: 2 attempts (one per D), `attempts_used=2`, `attempts_reserved=0`, 1 route |

Restart points: 1 creation (case 1), 2 reserved-before-claim (1), 3 after claim
(5), 4/5 D1 failure before D2 issue (1), 6 retained registration (3), 7 after
PASS acceptance (1; before acceptance: Coordinator test "repairs a crash after
pass CAS before route insert exactly once"), 8 during UNKNOWN (2), 9 after
owner resolution (2). Evidence: ledger and logs in `.tmp/stage4-actual`.

The acceptance found one defect, fixed before these runs: SearchState required
`one_of` on every requirement, so any real request with a presence-only
requirement could not be frozen (Rust `f0336685`, API `31b60f0a`, each with a
regression test). Harness defects are not counted as passes: the first CASE 1
run reused a source row whose object an earlier 3d run had deleted behind the
Coordinator's back (typed `source_unavailable`); cases now use unique sources.

Unit/integration: Rust `search_state` 7, receipt authority 7; API Runtime
Network suites 119 (including Stage 4 D1→D2 restart, frozen identity,
WaitForRuntime, CAS, presence-only requirement) and the shared canonical
SearchState fixtures byte-identical in the real WASM. Browser full E2E is not
claimed.

## Final stack regression (2026-09-25/26)

Final heads: ato `6a885970` (#1408) over #1407 `147c24be` over #1406
`ab667b82`; ato-api `b0236692` (#693) over #692 `422cd68b`. Hosts:
`oci-linux-test` (Linux aarch64, bwrap) and `ubuntu-sugamo` (Linux x86_64,
Docker 29) with per-tree targets. Unmerged and undeployed throughout.

| area | result |
|---|---|
| Local Formation Static / Python / Node (formation-worker suites) | 191 passed, 1 ignored |
| lowering parity (K/D refs + commands vs `lowering-before-2f.json`) | pass (7 fixtures) |
| common runtime (`ato-runtime-attempt`) | 58 passed, 1 ignored |
| formation lib (incl. SearchState, retained descriptor) | 193 passed |
| receipt/search authority | 7 passed |
| Portable Run Static / Process (`ato-cli` portable) | 15 passed |
| Portable Run OCI / service group (CLI, Docker) | Python process, OCI and service group: verified, served, confirmed stop, no containers left, scratch removed |
| Hosted validator (`ato-portable-application`, Hosted-like fixture) | 80 passed |
| Hosted LocalProcess etc. (`connected-realization-worker`, shim built) | 138 passed; Browser E2E fails (below) |
| Hosted OCI / service group / egress (Docker, lease start→finish) | 3 passed; nothing labelled left |
| Runtime Network Static / >32 MiB (40 MiB) Static / Python / Node | all satisfied, fresh receipts, VerifiedRoutes |
| Retained replay Static / 40 MiB Static / Python / Node | after deleting 14 source objects and a Coordinator restart: all satisfied, same K/D, new attempts and receipts, stored 0 |
| SearchState D1 fail→D2, UNKNOWN→resolve→next, retained, restart points, budget | Stage 4 acceptance above (cases 1–7) |
| API Runtime Network suites | 119 passed; typecheck; schema:check |

Browser: the Hosted Browser E2E needs `ATO_BROWSER_E2E_CHROME`; with the
host's Playwright Chromium it fails `webmcp ... slow_increment` (the fixture's
WebMCP tool is not registered). The stack changes no file in the Hosted worker,
the Browser adapter or the runtime-attempt Browser code, so this is not a stack
regression; it is not counted as a pass. No Browser full E2E success is claimed.

P0 remeasure (foundation regression, not coverage): 1/20 VerifiedRoute with
retained replay, 7 runtime capability (Go/Java), 6 build/toolchain, 3 execution
lowering (multi-service routes), 2 D authoring (pnpm pin), 1 source transport
(380 MiB); no K/receipt/UNKNOWN/publication defect. See
[P0 remeasure](formation-p0-remeasure-2026-09-26.md).

## Stage 4 completion gate

| # | gate | evidence |
|---|---|---|
| 1 | K freeze | 0300 write-once `frozen_json` + request trigger + 409; case 1 frozen identity unchanged across restarts; Coordinator test |
| 2 | deterministic candidate order | frozen D order in `decide_next`; WaitForRuntime test; case 2 continues to D2, not to a reordered D |
| 3 | durable cumulative budget | cases 1/2 `attempts_used` across restarts; case 4 `budget_exhausted`; 3c-a tests |
| 4 | durable UNKNOWN | case 2 held across restart and a polling healthy Runtime; resolution does not refund |
| 5 | source object transport | 3c-b; final regression 40 MiB source |
| 6 | retained object fresh replay | 3d; case 3; final regression Static/40 MiB/Python/Node after source deletion |
| 7 | fresh receipt authority | every PASS accepted via `accept_verified_route` (Coordinator WASM + requester) with a new attempt and receipt |
| 8 | D1 fail → restart → D2 PASS | case 1 |
| 9 | termination reasons separated | cases 1/4/5/6 (verified, budget_exhausted, candidates_exhausted, effect_unknown); owner_stopped from owner stop |
| 10 | Coordinator crash/restart consistency | restart points 1–9 (cases 1/2/3/5 + Coordinator test for 7-before) |
| 11 | no duplicated executor/K evaluator | K only via `verify_observed_candidate`; retained replay shares `CandidateLauncher`/`run_attempt`; TypeScript fallback rules removed |
| 12 | no AI decision in the deterministic core | `decide_next` is pure Rust over rows; no LLM/Jev/free-text patch |

All twelve are met at the implemented / locally-integration-verified level on
the unmerged stack. **Formation foundation v1 complete** is recorded in the
roadmap on that basis; it is not merged and not deployed.
