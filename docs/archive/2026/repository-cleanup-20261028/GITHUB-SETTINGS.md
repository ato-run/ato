# GitHub Pages / Deployments / Environment pre-state and cleanup record

Status: cleanup record. Pre-state captured 2026-10-28 before the GitHub-side
admin steps; the execution results in §3 were recorded after they completed.

Evidence: the read-only audit at `.tmp/repo-cleanup-20261028/audit/pages/`
(pages-deployments-audit.md, deployments.tsv, raw/ GET responses). The
authoritative per-deployment record is [deployments.tsv](deployments.tsv)
(133 rows + header, newest first), copied from that audit and refreshed immediately before deletion.

## 1. Pre-state (read-only, 2026-10-28)

### GitHub Pages

| Field | Value |
|---|---|
| `has_pages` | true (enabled 2026-05-04) |
| Site | <https://ato-run.github.io/ato/> — live on 2026-10-28 (GET 200) |
| `build_type` | `workflow` |
| `source` | branch `main`, path `/` |
| Custom domain / CNAME | none |
| HTTPS enforced | true |
| Public | true (public repository) |
| `homepage` field | `https://ato.run/about/` (recorded 2026-10-28; the earlier audit note "empty" was stale) |
| `status` | null (expected for `build_type=workflow`; state lives in deployments) |
| Publishing workflow | `.github/workflows/rfc-docs-pages.yml` (removed from the PR branch; main copy disabled_manually until the release promotion) |
| Workflow runs | 159 at audit time (124 success, 35 cancelled, 0 failed); run 40667376436 succeeded before disable |
| Cutoff — last run | id `40667376436`, 2026-10-28T09:50:23Z, success (a newer run than the audit snapshot) |

### Deployments

| Field | Value |
|---|---|
| Count | 133, all `environment=github-pages`, `ref=main`, `task=deploy` |
| Date range (UTC) | 2026-05-04T16:26:21Z .. 2026-10-28T09:51:52Z |
| Id range | `4572584920` .. `7382160739` |
| Latest states | 80 `success`, 7 `error`, 46 `inactive` |
| Creator | `Koh0920` (133/133) |
| Cutoff — last deployment | id `7382160739`, sha `08144622dc3801590167e8099ca42ddcaef71788` (= origin/main at purge time), created 2026-10-28T09:51:52Z |

### Environment

| Field | Value |
|---|---|
| Environments | 1 (`stg`, `production`, `dev` do not exist in this repository) |
| Name / id | `github-pages` / `14898949998` |
| Created | 2026-05-04T13:28:23Z (auto-created by Pages; never edited) |
| Protection rules | 1 `branch_policy` (id `53849917`): no required reviewers, no wait timer, no custom rules |
| Deployment branch policy | custom: `main` only (policy id `48597319`); `protected_branches: false` |
| `can_admins_bypass` | true |
| Other environment users | only `rfc-docs-pages.yml`; no release workflow declares `environment:` |

### Token / admin note (audit time)

`gh auth status`: account `Koh0920`, OAuth app token (`gho_…`), scopes
`gist, read:org, repo, workflow`. `GET /repos/ato-run/ato` reported
`permissions.admin=true`, so Pages disable, deployment deletion and environment
deletion are permitted with the existing token. REST rate limit at audit time:
4,646/5,000 remaining; the purge used roughly 400 calls. No DELETE/PUT/POST was
executed by the documentation pass; the actions in §3 were run separately by the
repository admin on 2026-10-28.

## 2. Executed sequence (repository admin)

Stage 1 is the repository change in PR #1623 (base `stg`): remove
`.github/workflows/rfc-docs-pages.yml` and the Docsify site assets. The PR was
opened before the irreversible steps so the pre-deletion record was published
first; `main` promotion remains a separate release action.

Stage 2 — disable Pages after the workflow was disabled on `main` (see caveat):

1. Preflight: confirm no queued/in-progress Docs Pages run and that the deployment
   count is still 133 with last run `40667376436`.
2. `DELETE /repos/ato-run/ato/pages` → expect `204`; `has_pages` becomes false and
   `GET /pages` returns `404`. On `409`, wait for in-flight builds and retry.

Stage 3 — purge the 133 deployment records (irreversible; [deployments.tsv](deployments.tsv)
is the preserved record). Oldest-first; mark a non-`inactive` deployment inactive
with `POST /deployments/{id}/statuses {"state":"inactive"}` before deleting it;
delete the current active deployment (`7382160739`) last.

Stage 4 — `DELETE /repos/ato-run/ato/environments/github-pages` → expect `204`.
If GitHub refuses the auto-created environment, use the UI fallback or leave it
inert and record the exception.

Stage 5 — verification: `has_pages=false`, `GET /pages` 404, environments
`total_count=0`, deployment count `0`, `ato-run.github.io/ato/` returns 404, and no
new "Docs Pages" workflow run appears after the next `main` push that touches
`docs/**` or `README.md`.

### Order actually used (and why)

The workflow removal is on the PR branch, not yet on `main`. To avoid a red
"Docs Pages" run on the next `main` `docs/**`/`README.md` push, the workflow was
first disabled on the default branch with `gh workflow disable 270860730`
(observed state `disabled_manually`). Pages was then disabled, deployment
records purged, and the environment deleted. The workflow file remains on `main`
until the release promotion, but it cannot run while disabled. Purge ran after
Pages was disabled so no new deployment could appear mid-purge.

### Accepted consequences

- The former Pages URLs (deep links into old `docs/` paths) stop resolving. This
  link loss is accepted; external bookmarks cannot be measured or redirected from
  this repository.
- Deployment history is not restorable. Only [deployments.tsv](deployments.tsv)
  and the audit `raw/` dumps (not committed) preserve it.
- `docs-ci` is unaffected (it runs local link and README checks only).

## 3. Execution results (2026-10-28 UTC)

| Step | Command / action | Expected | Actual | Timestamp (UTC) | Result |
|---|---|---|---|---|---|
| Preflight | read-only GETs, confirm 133 deployments and cutoff ids | unchanged | 133 deployments; last run 40667376436; last deployment 7382160739 | 10:16:28 | PASS |
| Stage 1 | open PR #1623 to `stg`; main promotion separate | PR open; no `main` change | PR #1623 open; workflow disabled on `main` | 10:15:53 | PARTIAL (main promotion pending) |
| Stage 2 | `DELETE /repos/ato-run/ato/pages` | 204 | 204; `has_pages=false`; `GET /pages` 404 | 10:16:35 | PASS |
| Stage 3 | purge 133 deployments (mark-inactive then delete) | 0 remaining | 133/133 deleted, 0 failures | 10:18:28–10:22:09 | PASS |
| Stage 4 | `DELETE /repos/ato-run/ato/environments/github-pages` | 204 | 204; environments `total_count=0` | 10:22:17 | PASS |
| Stage 5a | `has_pages`, `GET /pages`, environments, deployments | false / 404 / 0 / 0 | false / 404 / 0 / 0 | 10:22:30 | PASS |
| Stage 5b | `curl -o /dev/null -w '%{http_code}' https://ato-run.github.io/ato/` | 404 | 404 | 10:22:30 | PASS |
| Stage 5c | next `main` push touching `docs/**`/`README.md` | no Docs Pages run, 0 deployments | no new run after 40667376436; workflow disabled_manually; site 404 | 10:22:30 | PENDING next main push |

Do not record the token value or any secret in this file.
