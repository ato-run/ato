# Exploration D-conversion repair: actual OSS append

This append preserves the original 100-app result **baseline 7 → exploration 7
(+0)**. It is not an 8/100 result. Three successful cases establish **one unique
additional upstream app: SVGOMG**. Functional acceptance and persistence are
not measured. No source patch, K weakening, prompt/model/round increase,
permission ceiling expansion, deployment or remote migration.

## Existing evidence diagnosis

[Full offline diagnosis](formation-exploration-diagnostics.md) classifies all
274 opened rounds / 273 calls. The 84 no-valid-D apps partition by last observed
disposition: unexplained decline 30, inspection budget 14, final inspection 9,
unsupported entrypoint 8, unauthorized inspection 7, OCI builder 5, plan bounds 3,
dependency manifest 3, proposal schema 2, typed schema 1, digest 1, ceiling 1.
These are dispositions, not model-blame percentages. Original records cannot
prove actual newly transmitted text. Two canonical repeats stop no_progress;
38 semantic repeats and 22 execution-field changes are separately recorded.
Three unexecuted and five executed apps retain concrete per-app evidence.
Network recovery denominator is 0 → 0 → 0 → 0 → 0, authority 2 → 0 → 0 → 0 → 0.

## Generic changes from concrete failures

- Keep useful initial manifest/entry/config context in spare inspection slots;
  distinguish build configuration from tests/browser workers within the same
  four-file/16 KiB ceiling. Record exact transmitted text digest/bytes/truncation
  after provider input trimming. No private path/authorization-map disclosure.
- Typed execution_plan static_output lowers source-owned npm build to the
  existing ato.browser@1 server. Browser worker scripts are not Node servers.
  Static routes accept omission of unused guest_port; process routes still
  require explicit nonzero port. Optional diagnostic unknowns defaults to `[]`.
  Frozen K and canonical execution/requirements are unchanged by normalization.
- Empty asynchronous placement inventory waits for Runtime. A proven
  hard-filter refusal can still trigger repair. No producer round is spent
  just because registration has not completed.
- A positive owner-gate refusal stops a dependency/build step promptly, keeps
  execution facts and returns typed network_denied plus the phase gate report.
  Stop unconfirmed uses Abandoned/UNKNOWN. Reports are observational/bounded;
  absence never implies permission. No automatic grant.
- Two adjacent empty declines stop no_progress. Provider errors retain their
  separate budget. Unexplained legacy declines remain readable; new bounded
  unsupported reasons are available.

## Preregistered cases, including failures

| Condition / pin | Actual result | Calls / rounds |
|---|---|---|
| WBO, fb6f6bb7 | Valid npm D omitted network requirements; actual 403/retry waited through deadline. Requester expiry + harness JSON parse failure; Coordinator UNKNOWN. No app terminal/PASS established, never retried. |1 CP, no DP|
| SVGOMG zero-known-D, dfced918 | First D already valid; actual build/static server and fresh same-K PASS. Two subsequent premature calls occurred before Runtime result; this exposed placement bug. |3 CP/3 rounds|
| Copyparty, dfced918 | Unsupported Python dependency manifest, decline, invalid argv/plan bounds. No valid D/attempt. |3 CP/3 rounds|
| SVGOMG prior-live-D replay, dfced918 | Actual prior generated D `257d10b9` failed gulp build exit 127. Failure evidence reached all three live CP calls. New D `d57e640c` builds and serves static output; fresh same-K PASS. This is a separate seeded repair gate. |3 CP/3 rounds,1 known failure+1 generated PASS|
| SVGOMG schema/placement gate, 2ad8c108 | FAIL preserved: valid execution fields but missing unknowns diagnostic rejected as proposal_schema; later decline and invalid JSON. No valid D/attempt. |3 CP/3 rounds|
| SVGOMG normalized gate, 44772cb2 | First live-generated D `d57e640c` PASS. Round2 opens exactly after actual attempt finishes, so later calls are reduction attempts, not premature repair. Digest-mismatched reduction and decline leave successful D/receipt intact. No reduction executed. |3 CP/3 rounds,1 actual PASS|

All use original upstream archive/commit and the same frozen K. No post-result
substitution. Prompt v4 / deepseek-flash / thinking disabled throughout these
pilots. API JS is b20bba7a; matching native and Rust WASM hashes are in each
preregistered JSON. Model names are provider selectors, not immutable weights.
The seeded failure uses the byte-exact original live proposal with its stored
hash; no manufactured failure. Only one unique additional app is counted.

Final Runtime/CLI/authority source pin:
`44772cb2f1347a353a1d2c0593af4481c5da1e61`.
Final D: `sha256:d57e640c8f7e0c3cdfc483e644fc8b9023cdd06bb62b6c0f29d45aa8aa773ebc`.
Final fresh receipt attempt: `01M3SXDB4FM7AFA4124N2D8PNQ`.
Requirements: registry.npmjs.org:443 at dependencies only; ato.http@1 bind
app.http at runtime. Runtime egress denied; no state slots. Requirements/basis,
canonical K/D, receipts, per-round proposals and timing are in the JSON ledger.
Normal verified_routes remains 0; approval not_assessed, deployed false.
No mathematical-minimality or permissions-reduction PASS claim.

## Accounting and remaining gates

16 actual CP, 0 DP; input 122,127 / output 7,722 tokens. Estimate **45,919 USD
micros ($0.045919)**; conservative charge **157,296 micros ($0.157296)**.
Across the whole focused repair work, 16 calls and these costs per additional
unique typed-K PASS. This includes failures and duplicate acceptance cases;
zero-known SVGOMG first case alone cost 8,275 micros. Not extrapolated to 100.
DP was unnecessary (no ambiguous finite choice). Per-call latency is absent
from the current response journal; record case wall times without labeling
configured timeouts as latency. Model reservations unresolved 0; Runtime
UNKNOWN remains **1 WBO**, state and budget retained. No retry or fabricated
terminal. Network/authority recovery for upstream OSS is still unestablished.
The fail-fast boundary is code/real-gate unit verified, not a WBO recovery claim.

Remaining conservative model reservation: **564,906 USD micros**. A proposed
same 100-app wave requires worst-case 300 CP + 100 DP = **3,224,600 micros**. It is not
started or preregistered for execution. New budget, WBO UNKNOWN reconciliation,
exact-pin review and fresh run resources must pass before execution. The harness
rejects the proposed plan's unsatisfied gates before credential/resource access.
Do not use success from these subsets to overwrite or add to the old 100-app ledger.

Checks: core 20, worker proposal 38, actual gate 1, Runtime drain/cancellation 12,
Linux source-deleted retained replay 3 PASS. The accidental zero-test filter
was not counted. At preregistration head `47fe22944508dca1fd01ecfc9618bcaa1f2df0c6`,
CI run [36767358369](https://github.com/ato-run/ato/actions/runs/36767358369)
has Ubuntu PASS. macOS `a_process_run_is_owned_by_its_run_until_stopped`
fails with local Instance worker activation; cause remains unresolved, not
base-reproduced. Windows has 13 Unix API compile errors; referenced base source
lines are identical, which is not an exact-base execution. No all-green claim.
Draft #1452 / #712 retained; implemented/actual-subset-verified, merged=false,
deployed=false. General service/permission recovery and 100-app improvement remain
open. See exact-head CI notes in the final PR description.

## Raw evidence

82 files hash-verified against formation-exploration-repair-all-raw-manifest.json.
Archive 396,341 bytes, SHA256:
`e468e759ae1b5e355f3c2c8a42caaeb31d27f972ac54a74bc5262509dfa63ba0`.
Retained at the owned Linux exploration-metadata-repair-build and local
.tmp/exploration-diagnostics. Original 54-file archive and original 100-app raw
remain intact. Receipts, unknown state, frozen sources and final artifacts kept.
Reproduce JSON with scripts/acceptance/coverage/exploration-repair-evidence.py;
it checks raw hashes and receipt K/D/attempt identity before counting PASS.
