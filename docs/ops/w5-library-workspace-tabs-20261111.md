# W5 — Library Workspace tabs (2026-11-11)

## Shipped

- ato-api #1191: `GET /v1/workspaces/availability` (`{enabled, entitled,
  can_create, workspaces[]}`), creation gate (flag off → 404, not entitled →
  403 `workspace_entitlement_required`), `WORKSPACE_ENABLED` +
  `WORKSPACE_BETA_ALLOWLIST` bindings. Staging sets the flag on and allowlists
  the operator account.
- ato-pwa #801: Library → My Apps shows "Personal" plus one tab per organization
  when the API reports the surface; the tab renders the Workspace references in
  the existing My Apps grid. The header Workspace button and the standalone
  `TeamWorkspace` page are removed; `/workspace` redirects to
  `/?view=my-apps`.
- RFC: `docs/rfcs/draft/WORKSPACE_LIBRARY_TABS.md`.

## Verification

- ato-api: typecheck + `src/tests/workspaces.test.ts` → 9 tests passed
  (availability off, beta creation, non-entitled member read-only, org-owner
  coverage).
- ato-pwa: typecheck + `WorkspacesPanel.dom.test.tsx` +
  `GalleryHeader.test.tsx` + `shellPaths.test.ts` → 3 files / 28 tests passed;
  `npm run build` (89 static pages, 1 Journal page).
- Staging (`api 17840516-c942-4fc3-bf13-ff86952f3a34`,
  `pwa 67cc632e-13df-48f2-a2d2-8bab518095ff`), real Chrome:
  - `/workspace` → `https://stg-app.ato.run/?view=my-apps`.
  - `.header-workspace-button` count 0.
  - Tabs: `個人`, `Ato`, `Ato QA Workspace`.
  - `Ato` tab entries: Task Board, Calendar, Memos, Studio, Runners, GitHub;
    the Task Board tile opens the Instance in a new tab.
  - QA account B (member, not entitled): availability
    `entitled:false, can_create:false`, POST /v1/workspaces →
    `403 workspace_entitlement_required`; the QA Workspace is still visible
    because its owner is entitled.

Screenshot: `.tmp/w5-library-workspace-tabs.png`.

## Unified mode (follow-up, same day)

The first pass still rendered a separate `WorkspaceEntryGrid`. It now renders the
Library screen itself:

- ato-pwa #803: `MyAppsPage` takes an optional `workspace` mode and draws the
  references with the same `.home-screen`, `.app-grid` and `.app-tile` markup;
  the separate grid component was deleted. The personal listing, reorder and
  remove menus do not run in this mode; loading and failure appear inline.
- Validation: typecheck, `homeScreen.dom.test.tsx` + `WorkspacesPanel` +
  `GalleryHeader` → 3 files / 55 tests; `npm run build` (89 static pages).
- Staging `pwa d114017c-54c2-40fd-838b-8b1a24fa29f9`, real Chrome:
  - `/workspace` → `https://stg-app.ato.run/?view=my-apps`.
  - Tabs: 個人 / Ato / Ato QA Workspace.
  - Personal grid: 32 tiles (the normal My Apps screen).
  - `Ato` tab: **the same grid element** with 6 tiles — Task Board, Calendar,
    Memos, Studio, Runners, GitHub — and Task Board opens in a new tab.
  - Switching back to 個人 restores the 32 personal tiles.

Screenshot: `.tmp/w5-library-workspace-mode.png`.

## Creating a Workspace from the tabs

- ato-pwa #805: when the API answers `can_create: true`, the tab row shows a
  `+`; it opens a small dialog (organization select + name) over the same
  screen and calls `POST /v1/workspaces`. On success the availability is
  refreshed and the new tab is selected. A person without entitlement sees no
  `+` and the API still refuses with `403 workspace_entitlement_required`.
- Validation: typecheck, `WorkspacesPanel` + `homeScreen` → 2 files / 37 tests;
  `npm run build` (89 static pages).
- Staging `pwa de7ad8ef-4d92-43f6-b9b2-733b46a12dd1`, real Chrome:
  - Entitled account: `+` present; dialog lists `QA Workspace Org` and `Ato`
    with a name field and Cancel/Create actions (dialog opened and dismissed
    during verification; no workspace was created for the test).
  - QA account B: tabs show Personal and Ato QA Workspace, **no `+`**
    (`can_create: false`).

Screenshot: `.tmp/w5-library-create.png`.

## Known gaps

- Paid entitlement currently uses the existing cloud entitlement / paid-email
  grant as the proxy; a dedicated Workspace subscription does not exist yet.
- The QA organization still has three duplicate "Ato QA Workspace" rows from
  earlier E2E runs (tabs dedupe by organization, so only one shows).
- The Task Board tile opened a new tab; the log printed the URL before the page
  finished loading, so the URL itself is not in the captured line.
