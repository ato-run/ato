# AODD desktop Focus View evidence (claudedocs)

> Status: Archived; historical evidence of past agent operation runs.
> Language: ja+en (the receipts use the old YAML AODD schema, with Japanese
> observations). This is not current product state, an implementation contract
> or a deployment statement.

This collection is the earlier AODD (Agent Operation Driven Development) receipt
generation for the retired GPUI + Wry `ato-desktop` multi-window "Focus View"
shell. It was tracked at the repository root as `claudedocs/` and was moved
here unchanged on 2026-10-28, preserving per-file rename history.

| Measure | Value |
|---|---|
| YAML receipts | 50 (`receipt.yaml`) |
| Screenshots | 103 PNG |
| Files / size | 153 files / 171.1 MiB |
| Recorded | 2026-05-09 .. 2026-05-16 |
| References | ato#143, #144, #167–#174 |
| Subject code | retired 2026-08-28 (`d329323a6`); the Desktop product now lives in `ato-run/ato-desktop` |
| Former path | `claudedocs/` |

## Reading notes

- `aodd-143/` is the #143 omnibar receipt plus four screenshots.
- `aodd-receipts/` holds the focus-view iterations `r1`..`r42` (`r9` is
  absent), the launch-wizards, identity-button, `stage-a`..`stage-d` and
  `dock-wizard-e2e-20260516` receipts.
- Receipts use the old canonical AODD YAML schema from
  `.claude/skills/aodd/SKILL.md` plus ad-hoc keys. They are not the current
  Markdown receipt format; see the [AODD collection](../../../aodd/) for the
  retained Markdown generation.
- The ten repository-relative `evidence:` paths in
  [aodd-143/receipt.yaml](aodd-143/receipt.yaml) (4) and
  [aodd-receipts/ato-launch-wizards/receipt.yaml](aodd-receipts/ato-launch-wizards/receipt.yaml)
  (6) were rewritten from `claudedocs/...` to
  `docs/archive/2026/claudedocs/...` in the same cleanup. No other byte of any
  receipt or screenshot was changed. Every other evidence entry is a bare
  filename and stays valid because the directory structure was preserved.
- The planned `.DS_Store` deletion was a no-op: no `.DS_Store` file was
  tracked or present in the tree.

## Open items carried over

- Four receipts keep an unresolved `open` status: `focus-view-r1` (blocked),
  `focus-view-r3` and `focus-view-r40` (degraded), and
  `aodd-receipts/ato-launch-wizards` (complete but open). Their follow-ups were
  partly implemented and then removed with the shell; no current issue tracks
  them. They are retained as evidence, not as open work.
- Eight screenshots are not listed in their receipt's `evidence:` block: six in
  `ato-launch-wizards`, plus `r21/flush-pill.png` and `r39/search-input.png`.
- `dock-wizard-e2e-20260516` evidence strings point at `.tmp/` files that are
  not in the repository; there is no orphan copy to reconcile.
- Ownership question: because the Desktop product moved to
  `ato-run/ato-desktop`, these receipts may belong to that repository's
  evidence instead. This move keeps them in `ato-run/ato` history pending that
  decision. Nothing here is used by the current Desktop build, and
  `.gitignore` keeps ignoring new local `claudedocs/` output.

## Provenance

- Classification: all 50 receipts Historical (no active reference, no index
  entry, subject code removed). One byte-identical screenshot pair is retained
  deliberately: `aodd-143/evidence/02-after-omnibar-nav-to-example.png` and
  `03-keep-path-same-url.png` support different steps, so provenance is not
  interchangeable.
- Recorded in the [2026 repository cleanup report](../repository-cleanup-20261028/REPORT.md)
  and its `claudedocs-moves.tsv` (153 rows, one per file).
- Registered in the [2026 archive catalogue](../README.md).
