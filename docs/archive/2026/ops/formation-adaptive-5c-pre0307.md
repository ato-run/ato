# Formation 5c — implementation and actual merge blocker

**STOP / Draft / unmerged.** 5c is not completed. No new migration was created,
0301/0302 were not edited, no SQL fence was bypassed. Additional model calls 0.

## Pins

- ato base (#1434 merge): `857cb1ed4d71c99fde95a0302e0acf6952cdc7e8`.
- Core implementation: `20d825bd5ff09e1e2238a4975299e896093c545c` (#1435).
- Receiver tested: `0eb8eb5caaa24d3889cea5481e6005d3bb8b603d` (ato-api #707),
  base `7d61b969a2a0dbf075539c066913eb686a95bb1e`.
- WASM `eff1969064bd1a5a102c532fb5a757eb718913b9430dad4dc4982a46bfe6e342`.

## Local / actual evidence

Rust selected regression **675/0/1**, Clippy all targets, fmt and diff PASS.
Receiver **318/0** and typecheck PASS. Unit fallback tests do not exercise SQL.

Actual isolated Linux Coordinator/D1/Runtime/Verifier: A0, A1–A3, A4, A5,
A6 budget/deadline and A7 passed. A5 deliberately injects history-unavailable in
local D1 after a real FAIL; A7 pauses local proposal insertion across restart.
These are fault-injection integration, not naturally occurring effect failures.
Actual same-K receipts are embedded in JSON, with the frozen runtime constraint.
A9 observations exist, but the **complete A0–A9 gate is not closed**.

A8 failed to finish: the invalid injected choice label was refused by the HTTP
boundary, the durable decision timed out, the fixed producer was called once,
and its round completed/admitted D. No new decision or attempt followed.
The current C2 15-group re-run was not started because this blocker stopped the
suite. Historical C2 results are not relabeled as verification of this head.

## Root cause: existing SQL disagrees with Rust

0302 `trg_decision_open_guard` treats every fallback as pending while
`d.attempt_seq == current attempt count`. A completed proposal round adds D but
not an attempt. Rust correctly consumes the escalation once `proposal_round`
exists and returns the next OpenDecision; the DB silently ignores that insert.
Chosen escalation works because the chosen-action branch does not apply this
fallback predicate. This explains why positive actual A3/A7 and Rust-only
fallback tests pass while the actual fallback path stalls.

Fixing the durable fence needs an additive trigger migration with old-wire
compatibility and restart/concurrency tests. Do not weaken the Rust behavior to
skip the generated-candidate decision, fabricate an attempt, reinterpret old
fallback rows, or mutate 0302. The user explicitly required STOP if DB schema
changes became necessary; this PR and receiver #707 therefore remain held.

## CI classification

Rust CI head run 36511389959 compared with base 857cb1ed run 36510579404:
Windows Unix-only browser API errors, Ubuntu hosted Python/Node failure, and
macOS portable ownership/hosted validator failures also occur on that base.
No new failure in the fetched head failure set. CodeQL languages PASS at core
head. CI remains red, not green.

API Activity CI 36511529096: billing/spending-limit annotation before steps in
both jobs, full-serial skipped. No code execution; local results stand separately.

No live Jev/DeepSeek, credential read, deploy, remote migration, or #1421 change.
