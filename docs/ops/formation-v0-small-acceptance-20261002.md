# Formation v0 small acceptance — 2026-10-02

Status: in progress. Keep historical failures and WBO UNKNOWN unchanged.
No deployment, remote migration, feature flag or ordinary Run permission.

Merged foundation: Ato `8fca0f4b78b1bed92eae21823e5c5f7207058271`, API
`9e0031900836090612b94e775e26f980a68f32d7`. Latest code candidate:
Ato `e185f2d97d869615e23a83f950efb147d317f6b7` (explicit verification state
bindings, #1466); latest actual SVGOMG/OCI source execution used status retry
code `1de0e3a008f73226171d83fa026c50ec20dde976` (#1465), while
changedetection.io source/functional Runtime used
`3ac36fa2739cf8d9555026e93a4ca79905a6d4a7`. Both use API
`fa825a176b715962351466dc1a7149bec5979c7f`, prompt 12. Receipt authority
WASM SHA-256 `cc7445874bcea354b0ea03d96a71e10ea4e34b80b5516111457f1bdc92af3c03`,
2,196,846 bytes, source Ato `3ac36fa2739cf8d9555026e93a4ca79905a6d4a7`.
The [private build Record retention change](formation-build-record-retention-20261002.md)
is reviewed in Ato #1464 / API #718; API requires migration 0315, applied only
to the owned isolated acceptance database, not a remote environment.
Remaining state/input changes require a new final acceptance pin; earlier PASS
does not prove later changes. The review stack remains Draft.

## Measured previous pins

| Case | Provider | Execution pin | Result |
| --- | --- | --- | --- |
| SVGOMG | Codex session | Ato `3e60a9a5`, API `ace45da3` | First D failed required es5-ext install lifecycle; typed rebuild D passed frozen K. |
| SVGOMG regression | Codex session | Ato `3f07cd8e`, API `c7f990ce` | Same repair PASS, known D zero, three rounds / three session exchanges. |
| Single-service OCI fixture | Codex session | Ato `3e60a9a5`, API `ace45da3` | Zero known D, root Dockerfile build, ordinary Runtime launch and fresh K PASS. Excluded from OSS unique-app count. |
| changedetection.io | Codex session | Ato `3f07cd8e`, API `c7f990ce` | Missing feedgen wheel then native backend metadata failure; infrastructure failure, no sdist success yet. |
| Kutt | Codex session | Ato `3f07cd8e`, API `c7f990ce` | Native audit identified better-sqlite3/msgpackr-extract; expired temporary JWT stopped next candidate at admission. No native/migration/state success yet. |
| SVGOMG | API | Ato `3e60a9a5`, API `ace45da3` | First provider call returned HTTP 401. No candidate or Runtime execution. |
| Single-service OCI fixture | Codex session | Ato `90e09582`, API `b027f1c8` | Zero known D, root Dockerfile build, normal Runtime launch and fresh frozen-K PASS. |
| Kutt, final pin trial 2 | Codex session | Ato `90e09582`, API `b027f1c8` | Source inspection waited until the original round deadline; no D or Runtime attempt. Harness response delay, not application/native failure. |
| changedetection.io, final pin trial 3 | Codex session | Ato `90e09582`, API `b027f1c8` | Build got a zero-second limit because Worker subtracted a 45-second cleanup reservation before converting the remaining deadline to seconds. No dependency acquisition. Duplicate unchanged D in round 2 correctly ended as no_progress. |
| SVGOMG, trial 5 | Codex session | Ato `021ae956`, API `b027f1c8` | Zero known D, install audit failure then typed es5-ext rebuild; fresh same-K repair PASS. Three rounds / three exchanges; 273.161 seconds. |
| OCI fixture, trial 5 | Codex session | Ato `021ae956`, API `b027f1c8` | Zero known D, first generated Dockerfile D fresh same-K PASS. Two rounds / two exchanges; 143.314 seconds. |
| changedetection.io, trial 4 | Codex session | Ato `021ae956`, API `b027f1c8` | Missing feedgen wheel then sdist dependency acquisition exhausted the preregistered 128 MiB attempt transfer cap. Three rounds / four exchanges; 695.268 seconds. No sdist build/function PASS. |
| Kutt, trial 4 | Codex session | Ato `021ae956`, API `b027f1c8` | Typed better-sqlite3/msgpackr-extract rebuild and audit PASS; contained Runtime launched after source-owned migration with private JWT and isolated state. Two subsequent fresh receipts returned 302 against frozen root HTTP 200, so no typed-K or persistence PASS. Three rounds / five exchanges; 950.061 seconds. |

Trial-5 SVGOMG result SHA-256:
`edc71e9c687a62e1eb8fa1ccb1fb8093d2a8d6da7c225ae89570d398578f58e5`.
Trial-5 OCI result:
`459ad39c4000ebbf16e9b136f95d6e2ba993d44ecb731378a2b310277fc5f3da`.
Trial-4 changedetection.io result:
`dcb324364c86f4b35fc02d2f76714c713536cce7a42d6fb081d5641cc1e5a593`.
Trial-4 Kutt result:
`67f19ff67eedbebe6500e9f183f1f47a392e8be3a83a0eb9cf2011a9ecba5db2`.
All four used zero API calls; actual Codex provider calls/tokens/fees are unknown.
The 021 pin reused the 90e authority with an empty authority-source diff;
the new e02 budget classification changes Rust Search and requires the new WASM.

The Python transfer counter is shared across phases: its 134,229,426 bytes
against 134,217,728 allowed bytes must not be added three times. Positive budget
exhaustion was incorrectly reported as network_denied. The e02 fix preserves
the authoritative exhaustion through confirmed process cleanup, terminates
Search as budget_exhausted and keeps UNKNOWN dominant. Another fix preserves
the typed HTTP failure alongside successful native execution facts in the
bounded, redacted common reasoning input. Local validation: 33 compiler and
10 Search tests, two Worker network tests, process-group cleanup and failure
feedback regressions, all-targets Runtime/Worker clippy and formatting PASS.
API typecheck and 40 actual WASM tests PASS, including budget/restart/UNKNOWN.
Old trials keep their original cap, rounds and deadlines. A later higher-cap
acceptance trial must be a separately preregistered fresh Search in both arms.

## Runtime input and transport observations at 021

An isolated source-owned Python fixture requires PRIVATE_CONFIGURATION but
returns only its presence, never the value. It is excluded from OSS counts.
Frozen K: `sha256:8e8cbd060dcf9fb74ec944af6a28e192112546c51c90bf3f4c090fe3b61bed6f`.
Search `search_aabe9e9714d4be605188196695203ae0` paused as needs_input;
the actual form-input stdin route registered a reusable scope, and requester
resumed that Search with the original policy/journal/expiry. Fresh K PASS,
one Runtime attempt, two rounds / three exchanges (one schema repair).
Result SHA-256 `46763a7a9f31686685036d17af37a4da5a0203097d5911ddf44dfa6175b28fcc`.
Search `search_f9bdc238bf0343d3abf123748675d54e` independently started with
zero known D and reused the same scoped encrypted credential without input.
Fresh K PASS, one Runtime attempt, two rounds / two exchanges. Result SHA-256
`f844cf3674ef692ddc5bf083a1ab4a7a07793bd616c0ed078d086a3cd6a76ba1`.
Actual database metadata shows two assignments / two Searches / one credential.

With custom max_retries = 1, the real Coordinator committed Search creation,
proposal claim and Runtime PASS result before each response was deliberately
lost once. Requester and Runtime recovered these responses; both Searches
executed the fixture once. Original input round deadline was 21:55:02.643Z,
Search deadline 22:15:02.643Z and created_at 21:45:02.643Z on October 1 UTC;
registration did not change these values. Public files (96) and Runtime
files (926) were scanned against the private canary with zero matches.
This verifies runtime-only exclusion, not explicit embedding permission.
Scope-outside, ambiguity, revocation, expiry, browser input and embedding
allow/deny remain required. No external service credential was used.

An earlier fixture Search `search_00a0fc9dde14ec15bddbe44d1ebae1b1`
was interrupted by a harness KeyError while waiting for input. Preserve it:
no claim of application failure, safe replay, refunded reservation or PASS.
Its Runtime expanded/stored reservations were outstanding at that observation.
A later read-only Coordinator restart on the original isolated database at
22:16:13.625Z preserved the claimed attempt as UNKNOWN / result_not_received,
with no receipt/effects or resolution. It conservatively charged the 512 MiB
expanded/stored reservations and 128 MiB network reservation; no byte reservation
remains, no refund is asserted and no Runtime was restarted. These byte charges
are distinct from provider money. This new fixture UNKNOWN and old WBO UNKNOWN
are both preserved. The subsequent owned driver held
the Coordinator/Runtime alive through the owner's pause without changing
the original deadline.

The final-pin OCI Search is `search_cf32dc95fdbdf8d9cf80d0fd27ac418a`;
K is `sha256:e4fa58e07858ae71ce33b07fd016663ab3cb1713584685171dfb9c76f52ba862`,
D is `sha256:1456f7067b5732228a8186d5bbe8acd8d56e04829bcb163a21697cef31932d3c`.
Result SHA-256 `3d6f805cac8abbc7ebd7e7ecfdd26d1927acba4927325905c0627851047fa16d`.
First generated candidate PASS, two rounds / three session exchanges (one
invalid decline schema repaired within round 2), zero API calls. Session
provider usage and fee remain unknown. This is an infrastructure fixture,
excluded from the unique OSS application count.

A follow-up Worker control fix removes the cleanup reservation, rounds a
positive remaining millisecond budget upward only for the coarse build limit,
and retains the existing millisecond ExecutionControl cancellation. Cleanup
continues after expiry. Three deadline regressions and all-targets Worker
clippy PASS locally; real final application acceptance after this change remains
required. Old failed Searches are preserved; they are not given a new deadline.

SVGOMG frozen K: `sha256:6b2244326026e073d2c7d485434bb4d31d72fc43f06a3bc829c3dbfc6b1666d1`.
Successful D: `sha256:fae255ddb3921696fbf80dba4fadffe4226c6b8851f9f529ca6f25a6642927f0`.
Real headless Chromium through the common retained Runtime at Ato `3f07cd8e`
passed paste, optimize, smaller valid SVG download, Multipass toggle and reset,
with a fresh same-K receipt and confirmed cleanup. The original retained artifact
was created at `3e60a9a5`; this is a separate functional observation, not a new
automatic exploration or permission. Earlier selector/temporary-path harness
failures remain recorded and are not counted as application failures.

At that earlier checkpoint, historical unique OSS successes were Codex **1**,
API **0**. Codex SVGOMG is
a repair PASS, not first-attempt PASS. OCI is a separate infrastructure fixture.
No final-pin success rate or 100-case reachability rate is asserted yet.

## Later real observations and remaining materialization gaps

These observations supersede earlier claims that native execution, browser input
or embedding enforcement had not yet been exercised. Original trials above are
preserved at their actual pins; none becomes acceptance of a newer pin.

| Target | Actual pin | Observation |
| --- | --- | --- |
| SVGOMG, trial 11 | Ato `5143a6dc`, API `4af64035`, authority `5143a6dc` | Zero known D; first install audit FAIL, typed es5-ext rebuild fresh same-K repair PASS. Three rounds, three session exchanges. |
| SVGOMG function | Common retained Runtime `7d0822a1`; artifact creation `5143a6dc` | Fresh same-K PASS, SVG paste/optimization, smaller valid download (186 → 131 bytes), Multipass toggle/reset and cleanup PASS. Separate functional measurement; no ordinary Run grant. |
| Single-service OCI, trial 11 | Ato `5143a6dc`, API `4af64035` | Zero known D, root Dockerfile build, normal Runtime launch, first generated D fresh same-K PASS. Two rounds/two exchanges, 190.005 seconds; result SHA-256 `7d3a784b504e46c9c6774d40bc8280dbd5f455b1aa84c1ef63b1843b079cd112`. Infrastructure fixture, excluded from OSS counts. |
| changedetection.io, trial 11 | Ato `5143a6dc`, API `4af64035` | Unchanged requirements; 168 fixed wheels, isolated sdists, offline install and root fresh K PASS. Publication failed on the complete artifact size. One execution/two exchanges; no usable submitted D. |
| changedetection.io, trial 12 | Ato `d2d70601`, API `4af64035`, authority `5143a6dc` | Same K/D, root fresh PASS after native operations; gzip stored bytes 303,582,164, expanded regular bytes 558,325,853. Exceeds retained transport 256 MiB and expanded limit 512 MiB. No usable submitted D or functional PASS. |
| Kutt, trial 8 | Ato `e02d4ca3`, API `fe541c31` | Native rebuild/audit, private temporary JWT, source-owned migration and isolated state launch succeeded. Both receipts observed HTTP 302 against frozen root 200; no K/function/persistence PASS. |

SVG trial-11 Search `search_643977059bcd1da2e0eafb18f5d19e3b`, successful
attempt `01M3WW80ZMV1ZE9A2B5XSDRB8T`, retains the original SVG K and repair D.
OCI Search `search_36c14132a496577190c3928e39456789`, attempt
`01M3WW4NNEK4M6R78B3FVFSG5H`, retains the original OCI K and D.

Python trial-12 Search `search_5494c591e956590ee50186dc7ff9fcda`, attempt
`01M3WX0VEQJCMHBMR3K1KVECEC`, D
`sha256:298cb3ac5d58809ba45eb09ef182cbb05ac5fbd1ff55ddeaafbcd8e3c6f153df`;
result SHA-256 `2eaa0aa8cc976e23b5e66f44fafba92c47781718892c10c33728d018ae71f79e`.
One execution, two rounds, four session exchanges (one schema repair within
round 2), 784.368 seconds. Completed wheel archives use 137,102,363 bytes;
installed files use 402,724,732. There are no .pyc files. Read-only measurement
found only 1,153,971 bytes of large identical installed files, insufficient to
resolve the expanded overflow. No artifacts were removed or limits raised.
The build Record retention draft addresses this separately from execution.

Kutt's explicitly referenced root routing source redirects an empty local user
store to `/create-admin`; toggling anonymous-link policy does not initialize
that store. The repeated 302 is not a missing native tool or migration failure.
Connect source-owned local initialization and state bindings through the normal
Runtime, then verify the unchanged K and stop/restart persistence. No app preset,
secret-bearing D, weaker K or external service account has been added.

Real private-input observations are recorded in
`formation-embedding-failure-evidence-20261002.md`: PWA permission-required
409/explicit allow/save/fresh PASS; typed embedding refusal reaches common
reasoning; ambiguity stops for owner selection; TTL expiry stops before source
execution; selection/resume preserve deadlines and budgets. The final input
store has zero live metadata and zero encrypted values, and 215 public files
contain no protected configuration. Paid API calls are zero for these fixtures.
Earlier custom retry/lost-response/restart/UNKNOWN observations remain at their
separate pins. The old WBO UNKNOWN has not been replayed.

Current historical unique OSS apps with usable submitted D: Codex **2**
(SVG repair PASS and changedetection.io trial-14 first generated D PASS), API **0**.
The changedetection.io original Requester transport failure and report-only
publication recovery are recorded below; this is not final-head acceptance.
The OCI and private-input fixtures are not OSS app successes. Native claims
repair `70e8ba07` is proven by native integration and actual WASM tests; whole-app
acceptance remains required after the remaining materialization/state changes.

## Final code-pin native retention trials

Trial 13 at `3ac36fa2` / `fa825a17` used zero known D and unchanged source/K,
Search network ceiling 512 MiB / per-attempt 384 MiB, expanded/stored attempt
caps 512 MiB, retained caps 512 MiB expanded / 256 MiB stored. The first proposal
used state ID `data` with authority resource `app.data`; admission refused it
before execution. Its actual network evidence recorded zero bytes, while the
existing conservative Coordinator rule permanently charged its full claimed
384 MiB network reservation. A source-derived correction to `app.data` ran in
the same Search/round sequence, with the remaining 128 MiB attempt allowance.
Dependency acquisition exhausted that allowance at 134,228,580 shared counter
bytes; the three phase snapshots repeat that same counter and must not be added.
The original Search ended `budget_exhausted`, two rounds / three Codex session
exchanges, no submitted D, no paid API call. Preserve it unchanged:
`search_ca0eb15b97edd03692d310d24497f60f`; result SHA-256
`f233ee1e4e7bca2746118767b2dacbb2825811ee11b3ea886a50de7e4932b7e1`.
This is not a budget reset or a proven settlement defect: existing D1 invariants
charge full claimed network reservations across rounds and later requests.

Separately preregistered trial 14 used the exact same source/K/code/toolchains
and capacity limits, zero known D, original source-owned requirements and a
source-derived `app.data` state slot. Three sdists (feedgen, jstyleson, websockets)
built into a total 168 hash-fixed wheels, followed by offline installation,
normal contained Runtime launch and fresh frozen-K PASS. The first generated D
passed; a later Codex reduction answer made no further execution.
Plan SHA-256 `4af9ffaffdc8f589949d8f8fb7f7b21dcab5b8ac892ef8a2f0669e8955407d51`.

Requester status transport lost its connection after BuildRecord Ready, stopping
the original controller/Runtime mid-publication. Preserve its failure summary.
The confirmed finished/verified checkpoint resumed publication/reporting only,
without source acquisition, build, launch or inference. Original Search limits
and deadline are identical before/after; combined stored usage 306,002,627 bytes
equals BuildRecord 138,713,682 plus retained archive 167,288,945. Expanded usage
16,887,438 bytes; the dependency network shared counter is 157,248,045 bytes,
while the conservative claimed network debit remains 402,653,184 bytes.
BuildRecord `sha256:4b1e581775cc6e8a59a6bae4c2f82b424120f8ba1dd621e7168c3e9c95166ce8`;
retained `sha256:5a4563db6cd7a412c84ba31e2bcf123eff81be06d1514be61409fabf133a5f0d`.

That recovered attempt PASS did not itself settle the Search: the existing policy
allows a strict requirements-reduction round. A separate Requester resume opened
that common-input exchange; its read-only assertion failed and it was stopped
without another Runtime execution. Its logs remain preserved. The actual input
included `successful_derivation_ref` and the previous source-owned plan. Codex
answered `no_progress` because no supported strict authority/network subset kept
the same execution semantics. Search `search_fa9b884b3a7c0f73836ae5fc6bedf05f`
then became `satisfied`, two rounds / three session exchanges, one Runtime attempt,
zero paid API calls, no reservations outstanding. Result SHA-256
`19f5c89cfc45866c9dabe2a42fb2703eeb13669af0e69122c8fa2528e939a62b`.
The D awaits assessment; it grants no ordinary Run, deployment or public visibility.
Fresh retained Runtime replay and representative function checks completed
separately at Runtime `3ac36fa2`: original artifact digest and same frozen K
verified, actual Chromium 149 UI create/edit/pause/delete of an owned test watch
PASS, stopped Runtime cleanup PASS. Runtime network allowed zero bytes; external
fetch/change detection and stop/restart persistence are unverified. The corrected
harness uses the existing deny-all netd gate and Runtime bridge. Earlier missing
gate and UI modal-selector failures remain preserved, not counted as source/K
failures. Functional proof SHA-256
`fd6a1b2b2d61954322d019cd69fda5770f5a2379de3f4f1e122212126191507f`;
fresh receipt `0307bf218cf4ed9299c0c3054afdb84b0a77d0831a64345e02a0a310400e5e12`.
These source/receipt/function observations precede Requester retry code
`1de0e3a0`; final-head changedetection.io regression remains required.
Neither trial restarts old WBO UNKNOWN or supplies a Codex D to the API arm.

## Actual status-retry pin regressions

Ato `1de0e3a0` / API `fa825a17`, authority source `3ac36fa2`, Linux exact-pin
CLI/Worker/preflight binaries, zero known D, same source/K as earlier trials:

| Case | Result | Rounds / session exchanges / claimed attempts |
| --- | --- | --- |
| SVGOMG trial 15 | Authoring schema failure; no_progress, no Runtime execution. Preserved separately. | 1 / 2 / 0 |
| SVGOMG trial 16 | Correction PASS: HTTP authority admission refusal, then source-evidenced lifecycle audit, then typed es5-ext rebuild fresh same-K PASS. | 3 / 4 / 3 (one admission refusal, two actual executions) |
| OCI fixture trial 15 | First generated Dockerfile D fresh same-K PASS. Strict reduction answer made no further execution. | 2 / 2 / 1 |

The real Coordinator proxy dropped exactly one successful status response in
each SVG trial. Original custom one retry recovered via two durable sends;
no cached stale status, extra round, paid provider call or deadline/budget reset.
SVG Search `search_5177b730f7abfe243ed9469cdd2cb3aa`, result SHA-256
`e4a9012dd239eab568ccb553f3d8249df496dc28fb8a97e290cbd5cfcc7bda46`.
OCI Search `search_6bbd43b2d1a08575dc97a5eb1481ea41`, result SHA-256
`6f61ae5605803d867b94e1c2f54da651fe7e0e6f25f23d6a8263e6a741cc8cf4`.
Both are submitted, awaiting assessment, with zero outstanding reservations.
Combined final-pin fault proof SHA-256
`91f5474fbe2d469fffd84b723bf53443ad95c1c6037d960f11a0449c0d112a70`.
These regressions add no new unique OSS app. API-provider trials remain zero.

## Verification state connection

[Explicit Runtime state bindings](formation-verification-state-bindings-20261002.md)
at `e185f2d9` preserve assigned logical/private state resolution and prevent
candidate cleanup from deleting a borrowed working copy. Local Runtime 85 PASS
plus one added launcher rejection case PASS (86 distinct), Worker 75 PASS;
formatting and Runtime/Worker/CLI all-target clippy PASS. Existing API state
tests are 39 PASS / 1 failure, identically reproduced at merged main `9e003190`.
The recovery fixture reads an uncreated Run row; its failure is preserved.

The earlier Linux retained functional trial reuses changedetection.io trial-14
artifact without feeding it to the API provider. First and second actual
contained launches at `e185f2d9` passed unchanged K. The first non-PTY helper
received stdin EOF and confirmed cleanup before UI interaction; preserve that
receipt/cleanup evidence. A separately controlled second launch created, edited
and paused a test watch through Chromium, then confirmed stop. The third launch
found the same title and paused state in a fresh process and browser context,
passed unchanged K, deleted the own test watch and confirmed final cleanup.
All three fresh receipts PASS, zero allowed Runtime network bytes. Proof SHA-256
`f925202933e2e2ea25b4f00e118e5232650c4cc891a2b24abb63ad462790e5ac`.
Preregistration, additive harness correction and
helper versions are separately recorded. This is an explicitly owned local
state-binding fixture, not Coordinator state-assignment authentication. It does
not complete Kutt, current-owner/current-assignment or stale-fence acceptance.
The production Worker still has no borrowed state transport and does not
advertise persistent state on that unbound path. No ordinary Run/deploy grant.

Subsequent [Coordinator-backed verification](formation-coordinator-state-verification-20261002.md)
uses helper `e7e75b52`, controller `26eb1fbc`, common Runtime `e185f2d9` and API
`fa825a17`. Two distinct actual retained attempts pass unchanged K. Real
`dispatchRuntimeLaunch`/exact lease binding issue fences 1 and 2; confirmed stop,
authenticated revision commit/restore, fresh-browser title/pause persistence,
owned-watch deletion and final cleanup PASS. Nine production state-route
authorization/fence cases PASS. Two browser deletion harness failures and the
initial pre-execution receiver startup failure remain recorded. Stored private
state revisions total 55,296 bytes; active writers/quarantines zero; Runtime
egress bytes zero. Dedicated Coordinator stopped. Public metadata proof SHA-256
`d8ef5cc590d002cf25c93573ee3a8021a7fcd6ce056725c8bdc4228ca708eb52`.
This adds no unique app or paid provider call and does not complete production
source state provisioning, Kutt or the independent API arm. It supersedes the
local-fixture-only limitation for this measured transport round trip.

## Accounting

The explicit small-gate allowance is at most 24 additional API calls, within
the original remaining $0.499285; it does not authorize 100-case measurement.
Seventeen historical API call slots cost an estimated $0.065621. The one new
HTTP 401 call is `charged_unknown`, conservatively $0.017204, with unknown
actual tokens/provider fee. Total reserved call slots used: 18 of aggregate 41;
small calls remaining: 23 of 24. Conservative estimated total: $0.082825;
remaining total budget: $0.482081. Unsettled reservations: zero; one settled
charge remains an estimate with unknown actual usage. Never report it as free.
Codex session exchange counts are available, but actual provider call counts,
tokens and session fees are not exposed and remain null.

The existing private API credential returned 401. No repeated call with that
credential, alternate account, or plaintext key disclosure. Await the user's
authorized private file path; the call allowance remains in force. New API
trials must start from the same initial source/K/capabilities with zero known D.

## Remaining gates

Final-pin Codex then API application acceptance; changedetection.io external
fetch and persistence where explicitly granted;
Kutt native dependency, source-owned Runtime migration, temporary JWT, isolated
state and persistence after stop/restart; actual retry/deadline/lost-response/
restart/uncertain disconnect; scoped input reuse/revoke/expiry/cleanup and
embedding observations. These are not replaced by fixture/unit test PASS.
Current code-pin validation: Formation library 122 and compiler/exploration
integration 33 PASS; Worker library 75 PASS; Runtime Network 31 PASS / 1
explicitly ignored; authority 7 library / 8 integration PASS; process transport
2 PASS; retained replay 3 reported PASS with the macOS process containment
guard skipping actual Python/Node launch. Rust formatting/clippy and exact-pin
Linux CLI/Worker/preflight build PASS. Counterpart actual WASM and real isolated
D1/R2/Coordinator distinct cases 201 complete; migration/wire 15 PASS; API
typecheck and schema bootstrap PASS. CI absence at skip-ci heads is not CI PASS.

The base/head comparisons already recorded for original macOS/Windows/API/PWA
failures remain applicable to those tested pins; new unrelated failures require
new evidence. Old 100-case outcomes and WBO UNKNOWN remain preserved.
