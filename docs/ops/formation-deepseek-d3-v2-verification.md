# D3 v2 request evidence — verification, no live execution

[Preregistration](formation-deepseek-d3-plan-v2.md) ·
[Machine-readable ledger](formation-deepseek-d3-v2-verification.json)

Tested execution: `d1dde994c2d310a725b546921f9b3455777e28da`.
CI documentation head: `7dcd89dd972a10eeb61217209ac88b43c247a1c7`.
Subsequent changes in this PR are verification documents only.

| Check | Observed result |
|---|---|
| R1–R9 | All PASS |
| Legacy Cell/Response read compatibility | PASS; not accepted as v2 request evidence |
| Selected Rust regression | 668 passed / 0 failed / 1 existing ignored |
| Clippy (4 packages, all targets), fmt, diff check | PASS |
| Offline preregistration tests | 15 methods PASS, including N15–N19 |
| Rust helper synthetic journal inspection | PASS; duplicate fails closed |
| G0–G5 frozen projection reproduction | 6/6 identical to v1; no calls |
| M0–M13 actual loopback-provider integration | 14/14 PASS, receiver pinned to `38668a97…` |
| M13 completion loss + requester restart | 1 Request event, 1 response, 1 mock HTTP call; actual receipt PASS |
| C2 fixed actual integration | 15/15 groups PASS; fixed calls 12; model calls 0 |
| v2 controller E0–E19 | **Pending** subsequent #1431 update; not claimed from v1 tests |

M tests use isolated local D1/Coordinator and the real Python Runtime/Verifier on
Linux arm64. The 13 received synthetic model requests (M8 is connection refusal)
match their durable exact body hash; all 14 cells reserve once. There are no
DeepSeek network calls. This is not G0–G5 live acceptance or LLM efficacy.
The actual logs/raw mock evidence are retained under the task's `.tmp/d3v2/`;
the ledger pins their archive, result, request, source and binary hashes.
No source/assistant body or credential value was added to the budget journal.

## CI classification: do not call red green

Compared Rust CI head run `36457553330` with exact base `c3f44c6e…` run
`36432757237`, same workflow/platform. Full failure names are in the JSON ledger.

- Windows: same Unix-only browser-sandbox compiler errors on head and base.
- Ubuntu: same failure set (hosted common-process runtime, browser-state token
  test, and netd ingress tests) on head and base.
- macOS: portable ownership and hosted-validator failures match base.
- **Additional head-only macOS observation:**
  `activity_mcp_serves_all_fixed_tools_without_leaking_credentials_to_stdio`.
  `apps/cli/tests/activity_mcp.rs:191:44` reads a mock socket and gets
  `WouldBlock / Resource temporarily unavailable`; client then sees reset.
  The same base test passed. This is **not baseline-reproduced**.
  Three isolated local macOS reruns passed (1/1 each). CLI/MCP source is unchanged
  and does not invoke the DeepSeek adapter. Evidence suggests timing instability;
  local passes alone are not a causal proof. No unrelated code was changed.

The initial sandboxed loopback test run failed to bind localhost with EPERM;
rerunning with loopback permission passed. Linux's first build invocation lacked
Cargo on noninteractive PATH; using its explicit installed Cargo path succeeded.
Neither setup failure is recorded as a product failure or hidden as a PASS.

## Review boundary / remaining work

This request-evidence PR is Draft and unmerged for the requested review of the
new execution SHA and immutable v2 plan. #1431 remains Draft at `670d7a28…`,
unmodified. After v2 review/merge, update its consumer to Rust journal inspection,
keep preclaim projection only for source/K/G2 checks, and add E15–E19. Do not
claim v2 driver offline verification before those tests run.

Implemented: yes. Locally verified: yes. Mock actual integration verified: yes.
Merged: no. Deployed: no. Live calls: **0**. Spend: **$0**.
`DEEPSEEK_API_KEY`: **unread, including existence checks**.
No live journal initialization, G0 start, remote migration, deploy or #1421 change.
