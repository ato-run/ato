# Repository cleanup 2026-10-28 — documentation, evidence and Pages record

Status: cleanup record. This file describes what the 2026-10-28 cleanup changed,
what it deliberately did not change, and what remains undecided. It is not a
specification and does not certify deployment or verification state.

| Field | Value |
|---|---|
| Repository | `ato-run/ato` |
| Baseline | `2a3b75c332f9f01470436e828f7cf4d04d530840` (`origin/stg`, merged PR #1617) |
| Branch | `docs/repo-cleanup-20261028` |
| Worktree | `.tmp/repo-cleanup-20261028/ato` |
| Date | 2026-10-28 |
| Language | en (this report); moved evidence keeps its original language |

## 1. Scope

Three concerns were handled together because they touch the same tree:

1. Documentation ownership and status: say which surface owns which documentation
   and give unlabelled pages an explicit Current/Draft/Archived status.
2. Historical evidence: archive the tracked `claudedocs/` receipts and the
   superseded root `OPEN_ISSUES_ROADMAP.md`.
3. GitHub Pages retirement: remove the Docsify site assets and the Pages
   publishing workflow (separate change on the same branch) and record the
   GitHub-side pre-state for the administrator's follow-up.

Out of scope and not changed: runtime code, schemas, wire formats, deployment
configuration, accepted RFC bodies, ato.run application sources.

### Changes at a glance

| Kind | Count | Detail |
|---|---|---|
| Files modified (documentation pass) | 46 | 4 repository entry points, 1 docs index, 8 root/api technical pages, 7 concepts, 6 guides, 12 draft RFCs, 3 archived RFCs, 2 operations, 1 architecture, 1 internal, 1 archive catalogue |
| Files renamed with history preserved | 154 | 153 `claudedocs/` files + `OPEN_ISSUES_ROADMAP.md` |
| Renamed files also edited | 3 | two `receipt.yaml` evidence paths (10 lines) + the roadmap status banner |
| New files | 6 | `claudedocs/README.md`, this `REPORT.md`, `GITHUB-SETTINGS.md`, and three TSV records |
| Files deleted by this documentation pass | 0 | No stub, historical file or evidence file was deleted |
| Files deleted by the Pages change on the same branch | 9 | `.github/workflows/rfc-docs-pages.yml`, 7 Docsify/site files, `scripts/docs-site-check.mjs` (separate scope) |

## 2. Decisions

### 2.1 Compatibility redirect pages are retained (215 files)

The prior reorganisation (PR #1591) replaced moved pages with 215 redirect stubs
("This page moved to …", 146,814 bytes total) under `docs/ops/` (208),
`docs/` root (6) and `docs/specs/` (1). They are intentionally kept:

- 53 of them are still linked 79 times from 38 live files, and 5 are referenced
  from code, config or scripts (8 references), including
  `.github/workflows/rust-ci.yml`, `lib/formation/src/{intent,preset}.rs`,
  `scripts/ops/formation-worker-update.sh` and `tools/runtime-image-ledger/`.
- 162 have no inbound reference, but external issue and PR discussions quote
  their `docs/...` paths; deleting them turns those into 404s on GitHub.
- Deleting them requires repointing every reference and refreshing the reviewed
  link baseline first. That is a separate, reviewable change.

Decision: `retain-compatibility-stub` for all 215. No stub was deleted here.

### 2.2 Historical collections stay in place (781 files)

`docs/archive/2026/` already holds 328 archive-classified files, while 453 files
of the same kind still sit in live-looking directories (`docs/aodd/`,
`docs/internal/`, `docs/rfcs/archived/`, `docs/ready-state/`,
`docs/recipes/`, `docs/receipts/`, `docs/dev-notes/`, `docs/manual/`,
`docs/api/`, `docs/contracts/`, three `docs/ops/*20261028.md` reports).
They were not moved because moving them is not bookkeeping:

- the live `warmup-snapshot-build` skill and three Rust doc comments reference
  `docs/ready-state/`;
- `docs/internal/research/` and `docs/ready-state/` carry 20 of the 33 reviewed
  historical link failures;
- `docs/aodd/community-publish/` is read by
  `scripts/community_publish_successful_samples.py`;
- `docs/ops/*.json` evidence is read by acceptance scripts and tests;
- the prior inventory (`docs/archive/2026/documentation-reorganization/inventory.tsv`)
  is the frozen baseline that classifies those paths as Archive.

Decision: `retain-in-place` for 781 archive-classified files (328 already under
`docs/archive/` + 453 retained). No historical file was moved or deleted in this
pass. `docs/archive/2026/README.md` remains the catalogue that explains why the
tree looks mixed; the status labels added here make the "current vs historical"
split visible inside the live-looking directories that were left alone.

### 2.3 Docsify site assets and the Pages workflow are retired

`.github/workflows/rfc-docs-pages.yml` and the Docsify chrome
(`docs/index.html`, `docs/_sidebar.md`, `docs/_navbar.md`,
`docs/rfcs/index.html`, `docs/rfcs/_sidebar.md`, both `.nojekyll`,
`scripts/docs-site-check.mjs`) are removed in the same branch by the Pages
change. The Pages/Docsify site is retired from this repository change;
the GitHub-side disable is recorded in [GITHUB-SETTINGS.md](GITHUB-SETTINGS.md)
and executed separately, so the live site may keep serving until that step. The
former `https://ato-run.github.io/ato/` URLs are expected to 404 after it. That external link
loss is accepted and cannot be redirected from this repository;
in-repository links must keep working and are validated in §9.

Consequence for the claudedocs decision: the earlier objection that archiving
171 MiB under `docs/` would bloat the Pages artifact no longer applies once the
artifact is gone.

### 2.4 No ato.run repository edits

The ato.run documentation lives in `apps/ato-pwa/content/docs`. This cleanup
records the ownership split and the migration status but does not edit those
pages; see §3.

## 3. Documentation ownership and migration status

Ownership is now stated in [`docs/README.md`](../../../README.md) ("Documentation
ownership" and "Document status"), [`README.md`](../../../../README.md),
[`README.ja.md`](../../../../README.ja.md),
[`CONTRIBUTING.md`](../../../../CONTRIBUTING.md),
[`AGENTS.md`](../../../../AGENTS.md) and
[`docs/guides/README.md`](../../../guides/README.md).

| Surface | Owns |
|---|---|
| [ato.run documentation](https://ato.run/docs/) | General user documentation: Overview, What is Ato, Using apps, Library, CLI quickstart, Core concepts, Capsule, Use with AI |
| GitHub (`docs/`) | Architecture, RFC/ADR, protocol and implementation specifications, technical design rationale, operations, archive |

Overlap handling, with destination and status:

| GitHub page | ato.run destination | Status after this pass |
|---|---|---|
| `docs/run.md` | `/docs/cli-quickstart/` | **Retained as a technical reference.** General CLI installation/quickstart is owned by the ato.run page; `run.md` keeps the computation-root wire/profile lifecycle plus `reform`, `--initial-only` and bundle profile notes that ato.run does not carry. A pointer was added. |
| `docs/capsule.md` | `/docs/capsule/` | Partial overlap. `capsule.md` stays as the wire v2/v3/v4 table reference; a pointer to the ato.run page was added. |
| `docs/concepts/computation.md` | `/docs/core-concepts/` | Partial overlap. GitHub is richer (technical model); a pointer to the user-facing page was added. |
| `docs/concepts/capsule.md` | `/docs/capsule/` | Partial overlap; pointer added. |
| `docs/glossary-reference.md`, other `concepts/*` | — | GitHub-only (37 canonical terms vs a 9-term user subset). No migration. |
| `docs/guides/runner-hosting.md`, `docs/guides/connected-runner-self-host.md`, operations runbooks | — | **Pending ato.run migration**: no ato.run page covers Runner hosting or self-hosted Runners yet. They stay on GitHub until such a page exists. |
| Overview, Using apps, Library, Use with AI | ato.run | ato.run-only product UI; nothing to publish from this repository. |
| FAQ | none | ato.run has no FAQ page and this cleanup does not add one; no GitHub FAQ page exists either. |
| Publishing an app to the Store | none | User task with no current owner page: the GitHub page is a redirect stub over a historical guide, and ato.run has no equivalent. Recorded as unresolved (§11). |

Known content mismatch, reported and **not fixed** (ato-pwa owner): ato.run
`capsule.mdx`/`core-concepts.mdx` state that a Capsule "is identified by a
`ComputationRef`", while current GitHub docs and `AGENTS.md` root the
portable-application v3/v4 profile at `ContractRef`. Per repository policy the
user-facing page is not edited from a documentation-maintenance change.

## 4. Documentation inventory result

Inventory scope: 1,153 tracked docs files — the 1,152-file `2a3b75c33` baseline plus one operating reference added on `stg` before the rebase (827 Markdown), 33,122,959 bytes. The 153 archived `claudedocs/` files and the five new audit-record files are recorded separately (`claudedocs-moves.tsv` and this directory). Full per-file disposition:
[docs-inventory.tsv](docs-inventory.tsv) (1,153 rows) with columns
`path, category, decision, canonical_owner, status, rationale`.

| Category | Files | Decision used | Result |
|---|---|---|---|
| Technical-Current | 54 | `keep` (47), `retire-pages-asset` (7 site-chrome files) | 47 kept, 7 site assets retired with Pages |
| Technical-Draft | 101 | `keep` | 101 kept, 12 got their missing `Status: Draft.` line |
| Duplicate (compatibility redirect) | 215 | `retain-compatibility-stub` | 215 kept |
| Historical-Archive | 781 | `retain-in-place` | 781 kept (328 already under `docs/archive/`) |
| General-ato.run | 1 (`docs/run.md`) | `keep-technical-reference` | kept; general owner is ato.run |
| Unclear | 1 (`docs/coop.md`) | `keep` | kept; classification unresolved |

The inventory states decisions that were actually taken. It does not claim
deletions or moves that did not happen.

## 5. Status labels added

Added immediately after the H1, without touching technical text:

| Scope | Files | Line |
|---|---|---|
| Draft RFCs | 12: `FORMATION_DEEPSEEK_D3_PREREGISTRATION`, `FORMATION_DEEPSEEK_REQUESTER`, `FORMATION_EXPLORATION_LOOP`, `FORMATION_ROADMAP`, `FORMATION_SHARED_REASONING`, `HOSTED_BROWSER_COMPUTATION_V1`, `NATIVE_FRAMEBUFFER_RESIZE_BOUNDARY`, `NATIVE_HELPER_CONTROLS`, `NATIVE_TEXT_PASTE_BOUNDARY`, `OCI_EXPLICIT_NUMERIC_USER`, `RUNNER_WORKSPACE_CAPACITY_PREFLIGHT`, `RUNTIME_EVOLUTION_AUTHORITY_V1` | `Status: Draft.` |
| Operating procedures | `operations/formation-worker-build-identity.md`, `operations/single-jsx-compiler-provisioning.md` | `Status: operating procedure; verify the deployed binary and environment before use.` |
| Architecture explanation | `architecture/hosted-formation-capsule-v2.md` | `Status: Current architecture explanation; dated measurements are not a deployment statement.` |
| Internal guide inside an Archive-classified tree | `internal/testing-isolation.md` | `Status: current; hermetic test isolation.` |
| Current technical pages | `run.md`, `capsule.md`, `core-architecture.md`, `desktop.md`, `execution-identity.md`, `glossary-reference.md`, `snapshot.md`, `concepts/*.md` (7), `guides/*.md` (6) | `Status: Current technical reference.` |
| Archived RFCs without a status line | `rfcs/archived/ATO_CLI_STAGED_DEPLOY_RUNBOOK.md`, `ATTESTATION_FIRST_POST_APP_PERMISSIONS_RUNBOOK.md`, `STORE_WEB_STAGING_RUNBOOK.md` | `Status: Archived.` |

Deviations from the suggested list, recorded deliberately:

- `docs/api/snapshot-run-control.md` was in the suggested "Current technical
  reference" list, but the document itself declares "Historical coordination
  document" and the audit classifies it `Historical-Archive`. It received
  `Status: Historical coordination document; the in-body "staging-gated" status
  describes the retired control plane, not current state.` instead of a false
  Current label.
- `docs/rfcs/archived/ATO_CAS_STATE_MODEL.md` already carries
  `**ステータス:** Draft` in its body. No extra English `Status:` line was added,
  to avoid two conflicting status statements in one file.
- `docs/recipe.md` already carries a historical notice; `docs/coop.md`,
  `docs/theory/capsule-process-model.md` and `docs/connectors/PTY_V1.md` already
  declare Draft/non-normative status. All left unchanged.
- No status line was invented where a directory index already carries the
  classification (for example `docs/specifications/README.md`).

## 6. claudedocs archive

`git mv claudedocs docs/archive/2026/claudedocs` — 153 files, rename-preserving
(100 % similarity). Full per-file record:
[claudedocs-moves.tsv](claudedocs-moves.tsv).

| Measure | Value |
|---|---|
| Files | 153 (50 `receipt.yaml` + 103 PNG) |
| Size | 179,411,604 bytes (171.1 MiB) |
| Recorded | 2026-05-09 .. 2026-05-16 |
| Subject | retired GPUI + Wry `ato-desktop` "Focus View" shell (refs #143/#144, #167–#174) |
| Classification | all 50 receipts Historical |

- New [`docs/archive/2026/claudedocs/README.md`](../claudedocs/README.md) carries the
  status banner, the old path, reading notes, open items and provenance.
- The ten repository-relative `evidence:` paths were rewritten from
  `claudedocs/...` to `docs/archive/2026/claudedocs/...` — 4 in
  `aodd-143/receipt.yaml`, 6 in `aodd-receipts/ato-launch-wizards/receipt.yaml`.
  No other byte of any receipt or screenshot changed.
- One byte-identical screenshot pair is retained on purpose
  (`aodd-143/evidence/02-after-omnibar-nav-to-example.png` and
  `03-keep-path-same-url.png`); they evidence different steps.
- The planned `.DS_Store` deletion was a no-op: no `.DS_Store` was tracked or
  present in the tree.
- `docs/archive/2026/README.md` gained a "Retained collections" row.
- `.gitignore` line `claudedocs/` and `scripts/aodd-clean-host-postgres.sh` were
  left unchanged so future local receipts still land in an ignored directory.
  Because that pattern also matches `docs/archive/2026/claudedocs/`, the new
  README had to be force-added; the 153 moved files are tracked and unaffected.
- Not edited on purpose: `docs/ops/dev-consolidation-20261026/inventory-evidence.json`
  and `docs/archive/rfcs-legacy-draft/ADR-015-execution-contract-input-provenance.md`
  (both are frozen records of earlier work, including an already-dangling
  historical reference to a deleted `claudedocs/*.md`).

Open items carried over:

- Four receipts still say `open`: `focus-view-r1` (blocked), `focus-view-r3` and
  `focus-view-r40` (degraded), `aodd-receipts/ato-launch-wizards` (complete but
  open). Their follow-ups were partly implemented and then removed with the
  shell; no issue tracks them today.
- Eight screenshots are not listed in their receipt `evidence:` block.
- `dock-wizard-e2e-20260516` evidence strings point at `.tmp/` paths that are not
  in the repository.
- Cross-repository question: the Desktop product moved to `ato-run/ato-desktop`,
  so these receipts may belong to that repository's evidence. This pass keeps
  them in `ato-run/ato` history pending that decision.

## 7. Roadmap archive

`OPEN_ISSUES_ROADMAP.md` (24,669 bytes, 253 lines, Japanese body,
SHA-256 `ef70dc5b69b215c35447d32b93d5d1a41a7bee80d61777d024d1361009346f54`,
last content change 2026-09-23) moved to
[`docs/archive/2026/legacy/OPEN_ISSUES_ROADMAP.md`](../legacy/OPEN_ISSUES_ROADMAP.md)
with rename history preserved. A ja+en blockquote banner was prepended: historical
evidence / superseded roadmap, last content verification 2026-09-01, issue state
owned by GitHub, roadmap tracking moved to
<https://github.com/ato-run/ato/issues>, do not create new issues from the file,
and a pointer to the separate technical progress reference
[`docs/rfcs/draft/FORMATION_ROADMAP.md`](../../../rfcs/draft/FORMATION_ROADMAP.md).
The body was not rewritten.

No root compatibility stub was created: the file had zero inbound references and
zero GitHub mentions. Its former path is recorded in the archive "Moved documents"
table, and that table's intro now notes the no-stub exception. Roadmap tracking
now points at GitHub Issues/Projects from `README.md`, `README.ja.md`,
`CONTRIBUTING.md`, `AGENTS.md` and `docs/README.md`.

The frozen records (`documentation-reorganization/inventory.tsv`,
`inventory-evidence.json`) still name the old path; they are baseline snapshots
and were intentionally not edited, so they now disagree with HEAD for this one
file.

## 8. GitHub Pages, deployments and environment

Details and the operator checklist: [GITHUB-SETTINGS.md](GITHUB-SETTINGS.md);
the 132-deployment pre-deletion record: [deployments.tsv](deployments.tsv).

| Item | Pre-state (2026-10-28) |
|---|---|
| Pages | enabled, `build_type=workflow`, site <https://ato-run.github.io/ato/>, no custom domain, HTTPS enforced |
| Last Pages workflow run (cutoff) | id `40591978386`, 2026-10-27T22:04:53Z |
| Deployments | 132, all `github-pages`/`main`/`deploy`; 79 success, 7 error, 46 inactive |
| Last deployment (cutoff) | id `7371565372`, sha `f55c4b34…`, 2026-10-27T22:05:18Z |
| Environments | 1: `github-pages` (id `14898949998`), branch policy `main` only, no required reviewers |

Result fields are placeholders: Pages disable, the 132 deployment deletions and
the environment deletion are repository-admin actions and were **not** performed
by this documentation pass (no GitHub DELETE/PUT/POST was executed). The
workflow removal is staged in the working tree only and must reach `main`
through the normal release path before Pages is disabled, otherwise the next
`main` push touching `docs/**`/`README.md` would run a workflow whose Pages site
no longer exists.

## 9. Link validation

Commands run from the worktree root:

```sh
python3 scripts/docs-audit.py \
  --allow-historical-from docs/archive/2026/documentation-reorganization/link-audit.json \
  --output ../verify/docs-after.json
git diff --check
```

Output: `.tmp/repo-cleanup-20261028/verify/docs-after.json` (outside the
repository; not committed).

| Check | Result |
|---|---|
| Markdown files scanned | 924 |
| Local links checked | 1,708 |
| External links not fetched | 1,097 |
| Failures by scope | historical: 33 |
| Reviewed historical failures matched | 33 / 33 |
| Unexpected failures | **0** |
| Auditor exit code (with allowlist) | **0** |
| `git diff --check` | clean (no whitespace errors) |

The 33 historical failures are byte-for-byte the reviewed allowlist
(`link-audit.json`); this change adds no new broken active link and repairs none
of the allowlisted historical ones. Active-document failures remain 0.

Two failures introduced by an early draft of this report (three relative links
written with the wrong depth) were fixed before the recorded run; the run above
is the final state.

## 10. What was deliberately not changed

| Item | Reason |
|---|---|
| 215 compatibility redirect pages | External issue/PR path compatibility; repointing 79 references is a separate change (§2.1) |
| 453 historical files outside `docs/archive/` | Live script/test/skill references and the frozen baseline inventory (§2.2) |
| Accepted RFC bodies | Normative content; this pass only adds status lines |
| Runtime code, schemas, wire formats, deployment config | Out of scope |
| `apps/ato-pwa/content/docs` (ato.run pages) | Owned by the ato.run/PWA workstream; mismatch reported only |
| `docs/recipe.md`, `docs/coop.md`, `docs/theory/capsule-process-model.md`, `docs/connectors/PTY_V1.md` | Already state historical/draft status |
| `inventory.tsv`, `inventory-evidence.json`, `moves.tsv` under `documentation-reorganization/` | Frozen baseline records of the prior reorganisation |
| `.gitignore` `claudedocs/`, `scripts/aodd-clean-host-postgres.sh` | Keep future local receipts ignored / future receipt paths unchanged |

## 11. Unresolved items

1. **Compatibility stubs**: 215 stubs remain until the 79 live references (8 of
   them in code/config/scripts) are repointed and the reviewed historical link
   allowlist is refreshed. Deleting without that step breaks URLs and CI.
2. **Historical reorganisation**: the 453 files still outside `docs/archive/`
   need their own change with reference updates (`ready-state` skill and Rust doc
   comments, `aodd/community-publish`, ops evidence JSON readers).
3. **`docs/run.md` removal**: ato.run `/docs/cli-quickstart/` must first gain
   `reform`, `--initial-only` and the bundle/profile notes, or those paragraphs
   need a home beside the accepted `CAPSULE_CLI_LIFECYCLE` RFC.
4. **ato.run identity mismatch** (§3): `capsule.mdx`/`core-concepts.mdx` say
   `ComputationRef` where the portable-application profile roots at
   `ContractRef`; needs an ato-pwa owner and a deployed-content check.
5. **Publishing an app to the Store**: no current owner page on either surface.
6. **`docs/coop.md`**: non-normative, no adoption state; keep-as-draft or archive
   remains undecided.
7. **Duplicate ADR number**: `ADR-038-formation-generation-context.md` and
   `ADR-038-formation-verification-state-bindings.md` both use 038.
8. **claudedocs cross-repo ownership** (`ato-run/ato-desktop`) and the four
   open-status receipts.
9. **Pages administrator follow-up**: disable Pages, purge 132 deployments,
   delete the `github-pages` environment, then fill the result table in
   [GITHUB-SETTINGS.md](GITHUB-SETTINGS.md). Irreversible deployment deletion must
   not run before [deployments.tsv](deployments.tsv) is preserved in the merge.
10. **External Pages links**: former `ato-run.github.io/ato/` deep links stop
    resolving; accepted, cannot be redirected from this repository.
11. **Roadmap content**: archiving the roadmap leaves 45 unstarted tasks without a
    consolidated tracker. Issue state is owned by GitHub, but no issue currently
    carries the dependency ordering and Gates A–D.
12. **Runner/hosting documentation** has no ato.run counterpart yet (§3); the
    migration is pending an ato.run owner page.
13. **`.gitignore` pattern**: `claudedocs/` also matches the archived directory.
    The new README is force-added; if future work adds files under
    `docs/archive/2026/claudedocs/`, each needs `git add -f` (or the ignore rule
    would need a trailing-slash nuance that this pass did not change).
