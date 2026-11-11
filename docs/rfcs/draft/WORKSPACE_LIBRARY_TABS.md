# Workspace tabs inside Library

Status: Draft (2026-11-11)

## Decision

A Workspace is a usage context, not a second app surface. The product surface is
the existing Library → My Apps screen: its tabs switch between the personal
grid and the Workspaces the person can use. The standalone `/workspace` page and
its header entry are removed; `/workspace` redirects to the Library.

## Model

| Concept | Role |
| --- | --- |
| Library | Entry point for the apps a person can use |
| Workspace | A context that groups references to apps |
| Instance | The running app |
| DataResource | Data with its own ownership, independent of an app |

The tabs only change which references are listed. Switching tabs never copies,
moves, or re-permissions an app or a DataResource: a Workspace entry is a
reference (external URL or in-product route), and a personal/team tab may show
the same Instance.

## Flag and entitlement are separate

- `WORKSPACE_ENABLED` decides whether the surface exists at all. Off means the
  Library renders exactly the previous My Apps screen and the API reports
  `enabled: false`.
- Entitlement decides who may use it: a paid account (cloud entitlement or an
  active paid-email grant) or the `WORKSPACE_BETA_ALLOWLIST` user ids.
- Organization membership decides access to a team Workspace. A member sees the
  organization Workspace when an owner of that organization is entitled; the
  member does not gain creation rights. Removing the membership removes access.
- `GET /v1/workspaces/availability` returns `{enabled, entitled, can_create,
  workspaces[]}`. `POST /v1/workspaces` answers 404 when the flag is off and 403
  `workspace_entitlement_required` when the person is not entitled.

Creating and editing Workspaces is the paid/beta capability; reading an existing
team Workspace is not gated on the member's own plan. Entitlement changes must
never delete Workspaces or DataResources.

## UI

- The tab row shows "Personal" plus one tab per organization (the newest
  Workspace of that organization), only when the API says the surface exists.
- The screen IS the existing My Apps screen, not a second view: the same
  `MyAppsPage` component, `.app-grid` and `.app-tile` render the entries, and
  only the tile source changes (a `workspace` mode). Loading and failure are
  shown inside that same screen.
- External references open a tab; app/operations references are SPA routes. In
  workspace mode the personal listing, drag reorder and remove menus do not
  run — a reference has no instance lifecycle here.
- The row is not a second navigation header; the existing
  Explore / Library / Studio header is unchanged.
- Unconfigured back-office apps are not invented: a Workspace only lists
  references that exist.

## Degradation

If the availability call fails, the tabs are not rendered and the previous My
Apps screen remains, which is exactly the flag-off behaviour.
