# GitHub Pages / Deployments / Environment pre-state and cleanup record

Status: cleanup record. Written 2026-10-28 before any GitHub-side admin action;
the result fields are placeholders for the person who runs the admin steps.

Evidence: the read-only audit at `.tmp/repo-cleanup-20261028/audit/pages/`
(pages-deployments-audit.md, deployments.tsv, raw/ GET responses). The
authoritative per-deployment record is [deployments.tsv](deployments.tsv)
(132 rows + header, newest first), copied from that audit before deletion.

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
| Publishing workflow | `.github/workflows/rfc-docs-pages.yml` (deleted in this cleanup) |
| Workflow runs | 159 total: 124 success, 35 cancelled, 0 failed |
| Cutoff — last run | id `40591978386`, 2026-10-27T22:04:53Z, success |

### Deployments

| Field | Value |
|---|---|
| Count | 132, all `environment=github-pages`, `ref=main`, `task=deploy` |
| Date range (UTC) | 2026-05-04T16:26:21Z .. 2026-10-27T22:05:18Z |
| Id range | `4572584920` .. `7371565372` |
| Latest states | 79 `success`, 7 `error`, 46 `inactive` |
| Creator | `Koh0920` (132/132) |
| Cutoff — last deployment | id `7371565372`, sha `f55c4b34edd9960ba843c603c91e48a96ce09630` (= origin/main at audit time), created 2026-10-27T22:05:18Z |

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
4,646/5,000 remaining; the whole purge needs roughly 350 calls. No DELETE/PUT/POST
was executed by this documentation pass.

## 2. Planned sequence (owner: repository admin, not the docs writer)

Stage 1 is the repository change in this PR: remove `.github/workflows/rfc-docs-pages.yml`
and the Docsify site assets (done in the working tree by the Pages change on
branch `docs/repo-cleanup-20261028`).

Stage 2 — disable Pages, only after Stage 1 has reached `main`:

1. Preflight: confirm no queued/in-progress Docs Pages run and that the deployment
   count is still 132 with last run `40591978386`.
2. `DELETE /repos/ato-run/ato/pages` → expect `204`; `has_pages` becomes false and
   `GET /pages` returns `404`. On `409`, wait for in-flight builds and retry.

Stage 3 — purge the 132 deployment records (irreversible; [deployments.tsv](deployments.tsv)
is the preserved record). Oldest-first; mark a non-`inactive` deployment inactive
with `POST /deployments/{id}/statuses {"state":"inactive"}` before deleting it;
delete the current active deployment (`7371565372`) last.

Stage 4 — `DELETE /repos/ato-run/ato/environments/github-pages` → expect `204`.
If GitHub refuses the auto-created environment, use the UI fallback or leave it
inert and record the exception.

Stage 5 — verification: `has_pages=false`, `GET /pages` 404, environments
`total_count=0`, deployment count `0`, `ato-run.github.io/ato/` returns 404, and no
new "Docs Pages" workflow run appears after the next `main` push that touches
`docs/**` or `README.md`.

### Caveat — order matters

Do **not** disable Pages before the workflow removal reaches `main`. `main` would
still run `rfc-docs-pages.yml` on the next `docs/**` or `README.md` push, and
`configure-pages` would fail, turning that run red. Disabling after the removal
landed leaves only the site URL 404, with no failing workflow. Likewise, purge
deployments after Pages is disabled so no new deployment is created mid-purge.

### Accepted consequences

- The former Pages URLs (deep links into old `docs/` paths) stop resolving. This
  link loss is accepted; external bookmarks cannot be measured or redirected from
  this repository.
- Deployment history is not restorable. Only [deployments.tsv](deployments.tsv)
  and the audit `raw/` dumps (not committed) preserve it.
- `docs-ci` is unaffected (it runs local link/site checks only).

## 3. Execution results (to be filled by the operator)

| Step | Command / action | Expected | Actual | Timestamp (UTC) | Result |
|---|---|---|---|---|---|
| Preflight | read-only GETs, confirm 132 deployments and cutoff ids | unchanged | TBD | TBD | TBD |
| Stage 1 merge to `main` | release PR with the workflow/site-asset removal | merged, workflow absent on `main` | TBD | TBD | TBD |
| Stage 2 | `DELETE /repos/ato-run/ato/pages` | 204 | TBD | TBD | TBD |
| Stage 3 | purge 132 deployments (mark-inactive then delete) | 0 remaining | TBD | TBD | TBD |
| Stage 4 | `DELETE /repos/ato-run/ato/environments/github-pages` | 204 | TBD | TBD | TBD |
| Stage 5a | `has_pages`, `GET /pages`, environments, deployments | false / 404 / 0 / 0 | TBD | TBD | TBD |
| Stage 5b | `curl -o /dev/null -w '%{http_code}' https://ato-run.github.io/ato/` | 404 | TBD | TBD | TBD |
| Stage 5c | next `main` push touching `docs/**`/`README.md` | no Docs Pages run, 0 deployments | TBD | TBD | TBD |

Do not record the token value or any secret in this file.
