# Ato Space S4 — staging E2E (2026-11-11)

## Environment

- API staging active version: `72773317-184c-43ab-aff3-e3f63aeb16d3`
- PWA staging version: `1b84394e-730d-41bf-8a8d-3ae90e7991e3`
- Migrations applied on staging: `0413_space_invitation_reads`, `0414_space_invitation_workspace`
- Flag: `WORKSPACE_ENABLED=true` (temporarily set to `false` for the flag-off case, then restored and verified)
- Accounts: `qa-ws-b@ato-run.invalid` (owner, org `org_qa_workspace`), `qa-ws-c@ato-run.invalid` (invitee), `qa-ws-d@ato-run.invalid` (no organization)
- Spaces: `ws_ad5fbbb8d74845f6a7d1d0091c866cec` and `ws_qa_second` in `org_qa_workspace`

## Result

### Core Space flow — 14/14 PASS

Browser automation against the deployed staging PWA, with two real sessions:

```
PASS B: legacy URL lands on Library and shows the Space tab
PASS B: selecting the Space writes ?space=
PASS B: Share opens the member dialog with the invite form
PASS B: invitation is created and pending
PASS C: header bell shows the unread invitation
PASS C: invitation panel names the Space and the inviter
PASS C: joining adds the Space tab without reload
PASS C: member can read the Space manifest
PASS B: admin removes the member from the member dialog
PASS C: removed member loses the tab on the next sign-in
PASS C: removed member cannot read the manifest (404)
PASS B: re-invitation is accepted after removal
PASS C: declining does not add a tab
PASS C: declining leaves no manifest access (404)
```

### Additional staging checks

| Check | Result |
|---|---|
| Two Spaces in one organization render as separate tabs | PASS — `["My Apps","QA Second Space","Ato QA Workspace",…]` |
| Deep link `?space=` restores the selected Space | PASS |
| Mobile 390×844 tabs scroll horizontally (dark) | PASS — clientWidth 318 / scrollWidth 665 |
| Desktop light, desktop dark, mobile light, mobile dark | PASS — screenshots below |
| Re-login: brand-new browser context with the account's session | PASS — both Space tabs present |
| Unentitled account with no Space | PASS — no tab row, classic `My Apps` heading |
| Flag OFF (staging flag temporarily false) | PASS — no tabs, `My Apps` heading, `GET /v1/workspaces` 404, availability 200 `enabled:false`, `?space=` cleared |
| Flag restored to ON | PASS — tabs render again, invitation API 200 |

## Artifacts

- `.tmp/space-stg-b.png` — owner desktop, invite sent
- `.tmp/space-stg-c-joined.png` — member desktop after joining
- `.tmp/space-stg-c-removed-mobile.png` — removed member, mobile dark
- `.tmp/space-stg-b-desktop-two.png` — two Spaces, desktop
- `.tmp/space-stg-b-mobile-dark.png` — two Spaces, mobile dark
- `.tmp/space-stg-c-mobile-light.png` — member mobile light
- `.tmp/space-stg-flagoff.png` — flag OFF classic screen
- `.tmp/space-stg-d-free.png` — free account classic screen
- `.tmp/space-stg-c-relogin.png` — re-login with both Spaces

## Fixes found by this run

- `listPendingSpaceInvitations` / `cancel` / `resend` treated a workspace id as an organization id → 403 for owners/admins (ato-api #1202).
- `removeSpaceMember` / `leaveSpace` had the same mix-up (ato-api #1204).
- Invitations carried only an organization; the notification now names the Space via `invitation.workspace_id` (ato-api #1204, ato-pwa #828).
- `workspaceTabs` deduplicated by organization, so one organization could show only one Space (ato-pwa #830).

## Caveats

- Email delivery of invitations was not exercised (the in-app bell is the tested surface).
- `org_qa_workspace` still contains three pre-existing duplicate rows named `Ato QA Workspace`; they were left untouched. The duplication itself is why the tab-per-Space fix mattered.
- Login for the QA accounts used the staging Email OTP test path; no production data was touched.
- The staging flag was OFF for about three minutes during the flag-off case and restored immediately; the active version after restoration is recorded above.
