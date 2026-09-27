# c0 review corrections before c1

GitHub prerequisite check: #1419 OPEN, head
`6544253d00a3cf035218ad57dfd25604c0d97527`, no merge commit.
Fetched origin/main: `c9fc25fea5ed474c3d24ab64750645190b0f608a`.
This is the prerequisite audit SHA, **not a c1 implementation base**.

## Corrections confined to #1419

- Recognize only `coding:` and `coding=` cookie markers. Ordinary comments
  mentioning coding do not block default UTF-8 or hide a later actual cookie.
- Empty/malformed actual cookies, unsupported codecs, conflicting declarations,
  BOM conflicts and invalid UTF-8 continue to fail closed.
- Regression cases distinguish non-cookie prose (`# coding utf-8`) from an
  actual malformed declaration (`# coding=`). Both delimiters and prose followed
  by a real cookie are covered.
- ADR-039 clarifies `formation_failed` is a generic formation/execution fallback,
  not process-startup-specific evidence. No failure mapping changed.

The original c0 offline observations and E1 files remain byte-identical to the
pre-review head. This new record does not replace their historical counts.

## Verification and state

Four-package Rust regression: 568 passed, 0 failed, 1 existing ignored.
Context/2 tests: 10 passed. Clippy all-targets `-D warnings`: passed.

Implemented and locally/integration verified: c0 review corrections only.
Merged: no. Deployed: no. No API or migration changes.
Model calls: 0. Runtime efficacy reruns: 0. E2: not started.

c1 point/3, prompt/3 and provider integration have **not started**. The explicit
prerequisite is corrected c0 merged into main; this change does not bypass that
review/merge gate and does not auto-merge #1419.
