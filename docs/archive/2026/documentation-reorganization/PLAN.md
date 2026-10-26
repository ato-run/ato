# Documentation inventory and migration plan

Prepared before relocation on 2026-10-26. Baseline: `origin/dev`
`0c93d81a7f86140868fbe84efb63538360a9cba1`. Main already contains release
PR #1588 (`5df2f36d2f1d672b89fceb0e1fbb368a9835a12b`); #1589 brought the
main-only commit back to dev. This documentation change targets dev separately.

## Inventory

[inventory.tsv](inventory.tsv) records every tracked file under `docs/` plus
repository-wide Markdown, reStructuredText and README files: category, status,
language, title, bytes, baseline SHA-256, destination and action. Evidence
attachments are included, not just prose. Component READMEs stay with their
code. Architecture is a subcategory of Concepts; drafts are Specifications
with an explicit non-normative status. This catalogue classifies information;
it does not approve a draft or attest to deployment.

## Relocation and authority

[moves.tsv](moves.tsv) is the complete file-level relocation plan.

1. Keep all eight accepted RFCs in place and preserve their contents. Keep
   drafts in `rfcs/draft/`; keep the existing `rfcs/archived/` name rather than
   introducing a second `superseded/` tree.
2. Move ten reusable operating procedures from `ops/` to `operations/`.
   Move two implementation explanations to `architecture/`, explicitly
   distinguishing historical measurements from implemented behavior.
3. Move the remaining top-level `ops/*.md` reports and plans to
   `archive/2026/ops/`. Past preregistered experiments remain evidence, not new
   proposals. Leave JSON, logs, images, compressed evidence and the entire
   dev-consolidation evidence directory at their original paths. Preserve
   links and history; do not delete or rewrite raw evidence.
4. Move seven already historical root/specs pages to `archive/2026/legacy/`.
   Use `git mv`, then retain small compatibility pages at the former Markdown
   paths, including their old heading anchors. Rewrite navigational links in
   relocated prose relative to the new location; preserve historical code
   examples and measurements as evidence.
5. Preserve existing Ready-State, dev-notes, receipt, recipe evaluation,
   internal research/plan and legacy RFC trees. Classify them through the
   archive index and this exhaustive catalogue instead of moving established
   evidence subtrees unnecessarily.
6. Consolidate terminology in the existing glossary; expand Concepts with
   profile-specific identity, Formation/Verification, state continuity and
   compatibility. Correct explanatory current-status claims with code links.
   Record accepted-spec/code conflicts in a separate draft; do not resolve
   them by changing normative RFCs or implementation.
7. Make README, docs index, docsify navigation, Specifications, Guides,
   Architecture, Operations and Archive navigable. English is the default for
   current documentation; retain Japanese historical sources with language
   labels and a clearly named Japanese README.

## Validation and delivery

Plan refinement after command verification: five apparent host runbooks
(`connected-runner-max-slots`, `native-inference-vulkan-gpu-runbook`,
`sglang-cuda-host`, `pocketllm-demo-host`, `staging-snapshot-builder-sugamo`)
invoke the retired `ato runner` surface. Archive them alongside the other
historical ops records instead of promoting them as current procedures.
The final `moves.tsv` and inventory reflect five current operating procedures,
two architecture pages, and 208 archived pages (215 moves total).

- Compare local Markdown paths and heading fragments before and after the
  change, including compatibility anchors. Repair active navigation; catalogue
  unresolved historical references without inventing replacement evidence.
- Run the read-only ECC governance audit and distinguish its heuristic inline
  code-path warnings from real Markdown link failures.
- Inspect large JSON structure, sizes, Git history and inbound references.
  Record checksums and a retention proposal; externalization requires verified
  storage, access, retention and restore checks before any deletion.
- Scan documentation and evidence for credential patterns and personal-data
  indicators without printing sensitive values. Record review limits.
- Verify only documentation and documentation tooling changed. Application
  builds, deployments, migrations and feature flags are outside this PR.
- Commit the plan first, then relocations, then content and validation. Open
  an independent PR to dev. Merge only after review; main promotion remains a
  separate release action.
