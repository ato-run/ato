# Formation v0 small acceptance — 2026-10-02

Status: in progress. Keep historical failures and WBO UNKNOWN unchanged.
No deployment, remote migration, feature flag or ordinary Run permission.

Merged foundation: Ato `8fca0f4b78b1bed92eae21823e5c5f7207058271`, API
`9e0031900836090612b94e775e26f980a68f32d7`. Current execution candidate:
Ato `90e0958234702c22c91107b837deb43f1f34f8d9`, API
`b027f1c8c387b05839e221f93437235d47d0af80`, prompt 12. Receipt authority
WASM SHA-256 `aa3344df11da74fcb998ca54628f0d5267f87ae231310d26401f00dec4d846b8`,
2,119,970 bytes, source Ato `90e0958234702c22c91107b837deb43f1f34f8d9`.
The new pin requires new real application acceptance; earlier PASS does not
prove these later changes. The review stack remains Draft.

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

Current historical unique OSS successes: Codex **1**, API **0**. Codex SVGOMG is
a repair PASS, not first-attempt PASS. OCI is a separate infrastructure fixture.
No final-pin success rate or 100-case reachability rate is asserted yet.

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

Final-pin Codex then API application acceptance; changedetection.io function;
Kutt native dependency, source-owned Runtime migration, temporary JWT, isolated
state and persistence after stop/restart; actual retry/deadline/lost-response/
restart/uncertain disconnect; scoped input reuse/revoke/expiry/cleanup and
embedding observations. These are not replaced by fixture/unit test PASS.
Local regression evidence at the current pin: 33 compiler tests, 54 Worker
reasoning tests, 3 retained replay tests, real pip/npm Runtime-script operation
fixtures, macOS clippy, Linux binary/preflight build and 33 compiler tests;
API 8 encrypted-binding tests, 5 relevant Coordinator tests, 19 actual WASM
tests and typecheck PASS. CI absence at skip-ci heads is not CI PASS.

The base/head comparisons already recorded for original macOS/Windows/API/PWA
failures remain applicable to those tested pins; new unrelated failures require
new evidence. Old 100-case outcomes and WBO UNKNOWN remain preserved.
