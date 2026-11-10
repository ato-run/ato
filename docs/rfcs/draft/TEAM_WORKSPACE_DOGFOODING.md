# Ato Team Workspace — W0 audit and plan

Status: draft. Scope: dogfooding Ato itself as the team's working environment.
Staging first; no production migration, flag or deploy in this work.

## What already exists (checked 2026-11-10)

| Area | Reusable now | Not implemented / not deployed |
|---|---|---|
| ato-api auth | Better Auth organization/member tables (`drizzle/0064_organizations.sql`); fresh, uncached membership reads in `src/services/org_authz.ts` | Organization-owned DataResources (`owner_kind` allows only user/instance) |
| ato-api org library | `src/services/org_library.ts`, `src/routes/org_library.ts` — organization Capsule library entries and grants | A general Workspace entity (references, layout, pins) |
| ato-api DataResource | Operation-scoped grants, operation-only Ports, CAS + receipts, bindings, epochs, conformance (`src/services/data_resources/*`) | Organization Grants (`subject_kind` allows only user/instance); multi-Resource per Port |
| ato-api app builder | `app_builder_workspaces` (App Builder-specific minimal table) | Not a general Workspace; must not be extended unverified |
| ato-pwa | My Apps / Personal Apps shell, DataResources (`/settings/data`), Team Library surface, Desktop connection resolver | Workspace entry (Work / Back Office / Ato Operations), Personal↔Ato context switch, pins/shared layout |
| ato / CLI / Desktop | DataResource CLI with `--origin`; Desktop Local Coordinator serves DataResource and the PWA; Desktop My Data verified with CLI observing the same revision | Desktop workspace tabs/split view for team apps; Selfhost org support |
| ato-samples | `shared-json-todo` (operations incl. `todo_list_shared`/`todo_set_shared`), `shared-json-calendar` (`todo_list_due_dates`) | Samples PR #8 (base contract) and #10 (selected sharing) are still open, not on dev |

PR / environment state at audit time:

- ato-api PR #1155 **merged** (operation-scoped DataResource access, TODO→Calendar vertical).
- ato PR #1726 **open** (docs: v4 portable profile and saved-state K binding correction).
- ato-samples PR #8 **open**, stacked PR #10 **open** (selected sharing operations).
- Staging: API worker `e292afb5-8ab4-4e59-bfcc-3e47c5bafbaa`, PWA `460f6d14-6f31-43a2-a935-d8c4448179eb`;
  migrations 0404–0407 applied; `/v1/data-resources` answers 401 (route enabled).
- Authenticated staging E2E for the TODO→Calendar vertical has **not** been run (no staging test account yet).

## Workspace model

```
Organization   Better Auth organization/member — the only membership source
Workspace      usage context: app references, Resource references, shared layout, pins
AppInstance    running code + UI; may bind several Resources through named Ports
DataResource   logical data, Revision, Protocol; one Origin is the commit authority
Port/Binding   how an Instance uses a Resource; Grant decides authority
```

Rules:

- A Workspace reference is not permission. Every operation re-checks identity,
  organization membership and role, Instance entitlement, Port operation list,
  Binding, Origin Grant chain and epoch at the Origin.
- Workspace manifest holds references and layout only — never app data, long-lived
  credentials or secrets.
- `ResourceId`, `Revision`, `CapsuleId`, `InstanceId` and Workspace references stay
  distinct. A Resource update does not seal a Capsule.
- One Origin is the authority for a logical Resource; Hosted D1 and Selfhost
  SQLite are never implicitly synchronized and relay is not authority.
- Concurrent edits are CAS + receipt on one Origin: a conflicting revision is
  refused (409) and the user reloads; no silent merge. Writer transfer to another
  host is a separate concern.

## Screens

Three groups, in one Workspace entry:

| Group | Items |
|---|---|
| Work | Task Board, Calendar, Knowledge (a real running OSS app) |
| Back Office | Contacts/CRM, Documents, Invoices/Expenses — added as real OSS once each is verified; otherwise shown as **not set up**, never as finished |
| Ato Operations | Discover/Studio/Admin, Runner, GitHub/CI, Analytics, Support — links into existing privileged surfaces; no new admin app |

UI requirements:

- Workspace header states the context (Ato / Personal). Switching context moves no data.
- Shared apps reference Team Library entries and existing Instances; do not create
  a second copy of an app per member.
- Distinguish: runnable app, external link, needs setup, unsupported candidate.
- Pins and shared layout are separate from a person's view preferences and drafts.
- Desktop reuses its tab/split surface; mobile collapses to one app; no double header.
- Task Board All / Personal / Ato Team is a view and write-target selection over
  Resources; All composes observations and does not create a third source of truth.
  Keys inside the UI are `(ResourceId, TaskId)`.

## Resources

| Resource | Owner | Origin | Connection |
|---|---|---|---|
| Personal Task Board | user | the person's chosen default Coordinator | personal binding of the Task Board Instance |
| Ato Task Board | organization:Ato | an Origin chosen by an Ato admin | team binding held by each Task Board Instance |
| Other OSS data | the OSS app and its organization | the app's own persistence | reference/launch first; a Port only once an Adapter exists |

- Ports: `personal_tasks` (user scope) and `team_tasks` (shared Instance scope)
  reuse `ato.data-ports/2` and `ato.operations/v2`. Resource IDs never appear in
  app code or distributed Schemas.
- Organization ownership and organization Grants are prerequisites for the team
  Resource. The P1 migration question is recorded in
  `TEAM_SHARING_AND_WORKSPACES.md`: a tree rebuild of `data_resources` cannot run
  inside the D1 migration transaction, so P1 needs an explicit choice
  (coordinated rebuild vs companion table). Until that lands, the team Resource
  cannot be shipped.

## OSS policy

- Introduce existing OSS unchanged: launch via Formation, then reference from the
  Workspace. No rewrite of CRM/accounting, no unverified generic DataResource
  conversion.
- Record per app: upstream repo and license, purpose, chosen version, required
  runtime/dependencies, Origin and storage, external connections and secrets,
  start/stop, backup and export, sharing capability, authorization, acceptance result.
- If the OSS keeps its own DB, that DB is the authority; do not dual-write into a
  DataResource. A trusted Adapter exposes only the minimal operations later.
- Separate always-on shared apps from on-demand apps and measure start time, cost,
  concurrency and credentials. An app that launches but cannot persist is not adopted.
- Invoicing/accounting stay on sample data in staging; do not claim Japanese tax
  and retention compliance without verification.
- Ato Operations uses existing privileged surfaces; members without operator rights
  never see privileged admin functionality.

## PR plan

| PR | Content | Gate |
|---|---|---|
| W0 (this) | audit table, scope, cross-cutting spec in `docs/rfcs/draft/` | real apps, Origins, permissions and gaps identified |
| W1 | Workspace entry in the PWA shell: references, launch, pins, layout, Ato/Personal switch, operations links | real browser: open the Workspace and use an existing app plus an operations link |
| W2 | organization-owned Resource and authorization; Task Board personal+team bindings; Kanban/Tickets/Roadmap UI; Calendar operation-only connection | 2 accounts, 2 Instances, 2 Resources; revoke/conflict/compatibility negatives pass |
| W3 | one real Knowledge OSS adopted with persistence across restart; other categories added individually or marked pending | create → stop → start keeps data; unsupported candidates clearly marked |
| W4 | staging E2E of the whole Workspace, mobile, permissions, Origin outage, CLI/Desktop reach, measurements and runbook | PASS/FAIL/BLOCKED stated separately; artifacts usable day to day |

## Acceptance scenarios (must be evidence-backed)

1. Daily work: open Ato Workspace → create a Ticket in Task Board → check its due
   date in Calendar → create a page in the Knowledge OSS → quit, restart, data remains.
2. Ownership: personal tasks are unreadable to others; team tasks readable only by
   permitted members; deleting an App Instance leaves independent Resources intact.
3. One Instance, several Resources: switch/merge Personal and Ato without writing
   to the wrong one; ID collisions and a stopped Origin do not damage the other.
4. Different Instance, same Resource: another Task Board version edits a team
   ticket → same ResourceId/Revision/Receipt; incompatible versions fail closed.
5. Revocation: organization leave, Binding revoke, Grant revoke deny all new
   operations and past-result lookups; Calendar cannot upgrade to full read or replace.
6. Integrity: two concurrent writers → 409 CAS; lost response retried with the same
   operation ID; delayed write from an old tab; Origin disconnect → no double commit,
   no second authority, no false "saved".
7. OSS continuity: the chosen OSS really launches, persists, restarts, and its
   permissions, dependencies and cost are known; report BLOCKED instead of pretending.
8. Surfaces: Desktop and ~390px mobile, light/dark, keyboard, external links,
   error and permission-request paths.

## Non-goals

- Production migration, flag change or deploy; bulk migration of existing data.
- Real user invitations or sharing-scope expansion without separate approval.
- Tax filing, bank transfer, payroll, official ledger switch, real customer invoicing.
- Re-implementing CRM/accounting/support as Ato apps.
