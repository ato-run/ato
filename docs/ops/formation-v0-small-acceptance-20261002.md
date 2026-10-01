# Formation v0 small acceptance — 2026-10-02

Status: in progress. Keep historical failures and WBO UNKNOWN unchanged.
No deployment, remote migration, feature flag or ordinary Run permission.

Merged foundation: Ato `8fca0f4b78b1bed92eae21823e5c5f7207058271`, API
`9e0031900836090612b94e775e26f980a68f32d7`. Current execution candidate:
Ato `e02d4ca327c2dff6ef960863ebc6f1a24538b979`, API
`fe541c31111ec03b5b46ffda93cd5acb4036a5a7`, prompt 12. Receipt authority
WASM SHA-256 `38c2983a73d2ff25b47e80a345fdcb95b047bb8ce98990afffabd74899b766a2`,
2,119,998 bytes, source Ato `e02d4ca327c2dff6ef960863ebc6f1a24538b979`.
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
