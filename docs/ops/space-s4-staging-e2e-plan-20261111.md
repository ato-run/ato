# Ato Space S4 — staging E2E plan and runbook

Status: prepared, **not executed**. Staging deploy is blocked on Cloudflare auth
(`wrangler login`); nothing below has been run against staging yet.

## Prerequisites

1. `wrangler login` (or `CLOUDFLARE_API_TOKEN`) in the deploy shell.
2. Promote `dev → stg` for ato-api and ato-pwa; apply migration
   `0413_space_invitation_reads.sql` to staging D1.
3. Deploy API and PWA; confirm:
   - `GET https://staging.api.ato.run/health` → 200.
   - `GET /v1/spaces/invitations` with a session → 200 (flag on).
   - `POST /v1/workspaces` with a non-entitled account → 403
     `workspace_entitlement_required`.
4. Accounts: A (operator, entitled via beta allowlist) and B (QA account, member
   of a shared organization).
5. Two Spaces: S1 under the Ato preview organization, S2 under the QA
   organization. A is owner/admin in both; B starts as a non-member.

## Cases and expected results

| # | Case | Expected |
| --- | --- | --- |
| 1 | Library on mobile (390×844) | Tabs マイアプリ / Ato / ＋ in one horizontal row, no wrap; active tab underline; no duplicate "マイアプリ" heading |
| 2 | Desktop Library | Same tab row inside the pane; My Apps grid unchanged; help and expand controls on the tab row |
| 3 | Deep link `/?view=my-apps&space=<S1>` | S1 selected after load; reload keeps it; browser Back returns to マイアプリ |
| 4 | Legacy `/workspace` | Redirects to `/?view=my-apps`; no header Workspace button |
| 5 | A invites B to S1 (Share → email) | API 200/203; B's header bell shows unread badge; pending invitation listed with role |
| 6 | B opens bell | Invitation marked read; badge disappears; panel shows Space name, inviter, ［参加］［辞退］ |
| 7 | B joins | Tab S1 appears without reload (or after focus refetch); member list includes B |
| 8 | B declines S2 invite | No tab; invitation no longer pending; manifest request for S2 → 404 |
| 9 | A resends an invitation | Same invitation id, later expiry; B still sees one invite |
| 10 | A cancels an invitation | Pending list drops it; B sees no invite |
| 11 | Same Instance in My Apps and Space | Identical icon/label/colour in both; opening goes to the same Instance |
| 12 | A removes B from S1 | B's tab disappears on next focus/reload; `GET /v1/workspaces/<S1>` → 404 `organization_not_found` |
| 13 | Last admin tries to leave | 409 `last_admin_required`; tab stays |
| 14 | Re-login as B | Only Spaces with current membership are restored; deep link to revoked S1 falls back to マイアプリ |
| 15 | Non-entitled account | No Space tabs, classic My Apps only; `POST /v1/workspaces` 403; invitations endpoint still answers for own email |
| 16 | Light/dark, mobile Safari + Desktop Chrome | No unreadable labels; tab underline and badges visible in both themes |
| 17 | Task Board ↔ Calendar regression | Organization Resource still shared; Calendar shows due dates; stale whole-document write still 409; operation replay does not double-apply |

Screenshots per case: `.tmp/space-s4/<case>-<viewport>-<theme>.png`.

## Local mock harness (client gate, not staging)

`apps/ato-pwa/scripts/space-local-e2e.mjs` serves the built PWA with an SPA
fallback and mocks the API in Playwright. It exists because staging could not be
deployed while Cloudflare auth was unavailable; it is a development gate, never
a substitute for the staging run.

Run: `cd apps/ato-pwa && npm run build && node scripts/space-local-e2e.mjs`

Result (2026-11-11): **9/9 PASS**

```
PASS legacy /workspace redirects to Library
PASS tabs show My Apps and Ato
PASS header has no Workspace button
PASS bell shows unread badge 1
PASS opening the bell marks invitations read
PASS joining adds the Space tab without reload
PASS deep link restores the Space
PASS removed membership drops the Space and falls back to My Apps
PASS free account keeps the classic My Apps screen
```

The staging cases still to run are the ones that need real sessions, real
invitations and real org membership: accept/decline across two accounts, revoke
by an admin, re-login, and the mobile/desktop light/dark pass.

## Report format

For each case: PASS / FAIL / BLOCKED + screenshot path. Report the changed
authorization boundaries (Space → organization membership, instance grant,
Resource operation grant), and keep unexecuted cases BLOCKED rather than PASS.
