# Formation 5b-c0 — offline bounded information recovery

Base: `c9fc25fea5ed474c3d24ab64750645190b0f608a` (fetched origin/main).
Branch: `feat/formation-context-recovery-c0`.
Design: [ADR-039](../rfcs/draft/ADR-039-formation-context-recovery.md).
Machine-readable observations: [offline cells](formation-context-recovery-c0-2026-09-27.json).

## State separation

- **Implemented:** explicit context/2, fixed delegation/encoding/scan fields,
  audited lifecycle failure vocabulary, frozen-source offline requester API.
- **Locally/integration verified:** core projection and requester snapshot/privacy
  tests, historical provider compatibility and four-package regressions below.
  These are offline/library/integration tests, **not an actual Coordinator gate**.
- **Merged:** this slice is unmerged; review required. E1 #1417 is merged at
  `c9fc25fea5ed474c3d24ab64750645190b0f608a`.
- **Deployed:** no. No API changes, migration creation/application, staging,
  production, feature flags or provider-key operations.

Model calls **0**. Actual Runtime/Coordinator acceptance reruns **0**. Ordinary
Rust regression tests use their existing local test helpers/mocks/process tests;
none reruns E1 or its oracle. No E2, selector promotion or new generated D/receipt.
Provider remains point/2 + prompt/2 with context/1. Context/2 cannot deserialize
as context/1 or enter point/2. Provider point/3 and prompt/3 are deliberately
**not implemented** until a separately authorized provider-integration slice.

## Offline observations

The Rust test constructs the existing E1 candidate bytes and asserts every
candidate SHA-256 against immutable preregistration before projection. It does
not run Python, the Runtime, a selector or a model. Assertions about positive
candidates stay in tests, not in production projection. Both opaque-ID mappings
(`q7`, `m2`, swapped) are evaluated.

| Case | Both permutations | Recovered information | Remaining uncertainty |
|---|---|---|---|
| E04 | 2/2 | wrapper python_main delegation; negative none | target/behavior not proven |
| E06 | 2/2 | bounded_prefix server marker vs complete negative | both contain server markers; suffix unknown |
| E07 | 2/2 | complete Latin-1 projection with server marker vs UTF-8 negative | encoding itself says nothing about K |
| E05 | unchanged equality except ID | no status-literal information | intentionally indistinguishable |
| E09 | unchanged equality except ID | no oracle addition | intentionally indistinguishable |
| E10 | no service marker | both remain CLI-like | not a behavior proof |

Thus expressiveness gate **6/6**, not an efficacy gate. E06/E07 metadata differs
but c0 has not shown how to choose between their equivalent service hints.
E1 remains A 0/20, B 9/20, C 2/20, robust additional success 0, efficacy unmet.
5b remains In progress; general 5b-c and E2 remain on hold.

## Failure audit and privacy

New context/2 mappings (no message parsing):
`candidate_not_observable` → same closed code;
`formation_failed` → same closed code;
`candidate_stop_unconfirmed` → same closed code;
`candidate_cleanup_failed` → same closed code.
See ADR-039 for actual producers and meanings. Existing codes stay unchanged;
all other codes become `other`. In particular no evidence supports changing
E08's historical `other` to a guessed process-exit code.

Tests cover comment/string-only delegation, target-path suppression, source
mutation after freezing, raw messages/host fields, `IGNORE PREVIOUS INSTRUCTIONS`,
`sk-test-private`, `https://private.example`, `PRIVATE_PATH_CANARY`, UTF-8 BOM,
explicit Latin-1 aliases, malformed/conflicting/unsupported cookies, invalid
UTF-8, unsafe string/bracket/name/continuation cuts, suffix independence,
read-size mismatches, entry/context/count bounds, duplicates and unknown fields.
No source text, filename, raw name, URL, argv, receipt or secret is projected.

## Verification

Commands (repository root):

```sh
cargo test --locked -p ato-formation -p ato-runtime-attempt -p ato-formation-worker -p ato-receipt-authority
cargo test --locked -p ato-formation --test generation_context_v2 -- --nocapture
cargo clippy --locked -p ato-formation -p ato-runtime-attempt -p ato-formation-worker -p ato-receipt-authority --all-targets -- -D warnings
```

Final four-package regression: **567 passed, 0 failed, 1 existing ignored**.
Formation 261 (including context/1 15 and context/2 9), worker 238 (including
provider compatibility 24 and requester 17), runtime-attempt 58, receipt-authority
10. Clippy all-targets with `-D warnings`: passed. Logs live in the worktree's
`.tmp/c0/`. No manual CI rerun.

Four mutation probes independently disabled delegation, prefix recovery,
Latin-1 support, and candidate-not-observable normalization. Each failed its
corresponding assertion (not a compiler error); original code was restored
before final regression/clippy. The JSON ledger records the detected mutations.

## Historical evidence preservation

E1 plan, amendment, reservations, raw/result/summary JSON, oracle and evaluator
files are unchanged; no reaggregation. The only edit to its Markdown results
ledger is an explicitly labeled post-merge current-state correction for #1417;
no historical measurements or conclusions changed. #1402 remains open and
conflicting (head `2c1c6924286782f090783f1bd47a68e2e9b979ba` at audit); neither
merged nor edited. The Roadmap retains E1's negative result and separates c0.
