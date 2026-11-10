# Team sharing and workspaces (draft)

Status: draft. Scope: sharing one TODO DataResource with selected team members,
organization-owned Resources, and workspace composition.

## Why

Ato's DataResource is independent of any application: the same board can be
edited from two different Task Board instances, and a team can own a board that
outlives any one member's account. The current implementation already supports
that at the operation boundary — an instance whose Port lists only
`todo_list_due_dates` cannot read the document — but sharing is still expressed
as a Grant to one instance. Teams need:

1. selection: the owner shares specific tasks, not the whole board;
2. membership: any member of a team can use the shared view, including members
   who join later;
3. ownership: the Resource belongs to the organization, not the person who
   created it;
4. composition: one instance shows multiple Resources (personal + team) without
   creating a third source of truth.

## Model

```
Organization   team主体・所有権・メンバー（Better Auth organization/member）
Workspace      利用文脈: アプリ参照・Resource参照・共有レイアウト
AppInstance    実行コード・UI（複数 Resource を Port で参照できる）
DataResource   論理データ・Revision・Protocol
Port/Binding   Instance が Resource を使う接続点（Grant で権限を確認）
Grant          owner が subject (user / instance / organization) に与える権限
```

Rules that do not change:

- A Grant is checked on every request; it never propagates to another instance.
- An operations-only Port can only invoke listed operations; document read and
  replace stay refused.
- Revocation takes effect on the very next request (no membership or grant
  cache).
- A Port binds one Resource. Several Ports (e.g. `personal_tasks`,
  `team_tasks`) may bind several Resources to one instance; the UI composes the
  view and never writes a merged document.
- Task identity across Resources is `(ResourceId, TaskId)`; a write names the
  Resource, its Binding and the expected revision. Conflicts are refused and the
  user reloads — no implicit merge and no second authority.

## Phases

### P0 — selected sharing on the operation boundary (done)

- Sample TODO: `todo_set_shared` (evolution) and `todo_list_shared`
  (observation) return only `shared=true` tasks with `task_id`, `number`,
  `title`, `status`, `due_at`.
- Real SQLite Coordinator E2E (`services/coordinator/e2e/team-sharing.mjs`):
  the consumer instance sees one task and no private fields; an ungranted
  operation is 403; unsharing empties the shared view while the board, both
  tasks and the revision remain; re-sharing restores the view.
- This is the consent boundary. It does not yet make the Resource
  organization-owned, and the Grant still goes to one instance.

### P1 — organization-owned Resources

Needed because a person leaving must not take the board away.

- `data_resources.owner_kind` currently allows `user` and `instance` only, and
  SQLite cannot drop a CHECK constraint. The migration must rebuild the table
  (create new, copy rows, drop, rename) with foreign keys pointing at it handled
  explicitly, then recreate indexes and triggers. It is forward-only and needs a
  staging rehearsal before it is applied to production.
- Add `billing_organization_id` so capacity is charged to the organization, and
  keep `billing_user_id` as the actor that created it for audit.
- Authorization for `owner_kind='organization'`: the acting user must be an
  active member of the organization (fresh read of `member`), and management
  (grant/revoke/delete) additionally requires `owner`/`admin` from
  `org_authz.ts`. A non-member receives the same uniform 404 as today.
- Only `document` backend Resources may be organization-owned; instance-bound
  legacy state keeps its current owner.

### P2 — organization Grants

Sharing to a person's instance does not scale to a team: the owner would need
every member's instance id, and a member who joins later would get nothing.

- New table `data_resource_organization_grants` (resource_id, organization_id,
  capability bits, `operations_json`, status, grant_epoch, granted_by_user_id).
  It is deliberately separate from `data_resource_grants` so the existing table
  (and its CHECK) does not need a rebuild.
- Authorization unions it: a request may satisfy the operation through an
  organization grant when the actor has a fresh `member` row for that
  organization, and the grant's operation list contains the requested operation.
- Creating or revoking an organization grant requires the resource's owner (or,
  for organization-owned Resources, an org admin) and is audited.
- Binding creation from an organization grant is allowed only for an instance
  whose owner is a current member; the binding stores the grant id and epoch, so
  the existing epoch checks still detect a revoke between read and write.

### P3 — workspace composition

- A Workspace stores references (app instance, Resource ids) and a shared
  layout. It grants nothing: a reference is not permission.
- Shared layout + per-user overlay: the team's Resource references live in the
  shared part; personal Resources and view preferences live in a per-user
  overlay. Joining a workspace never shares personal data.
- A workspace manifest is not yet a DataResource. Saving it as one is allowed
  later but adds no new computation primitive.

### P4 — multi-Resource Task Board

- The Task Board instance binds `personal_tasks` and `team_tasks` with separate
  Grants. The combined list is a view over two Resources; no third document.
- Writes route to the Resource that owns the task; CAS conflicts surface as a
  reload prompt.
- A second Task Board implementation (different code/design) that supports the
  same `todo.board@1` operations can open the same team Resource. Compatibility
  is decided by Protocol + declared operations, not by CapsuleId or code hash.

### P5 — dogfooding (Ato team)

1. Create a team workspace and an organization-owned Task Board Resource.
2. Show the personal Resource and the team Resource in one Task Board instance.
3. From another account, use a different instance to operate the team Resource;
   personal Resources stay refused.
4. Try member removal, grant revocation, app update and CLI/Desktop edits; data
   survives and authority is lost exactly when it should be.

## Coordinator and Origin

- Origin (D1/SQLite) owns the bytes, CAS, receipts and trusted operation
  handlers.
- Coordinator owns location, writer lease/epoch and grant/transfer mediation.
- The first implementation may run both in one service; the responsibilities
  stay separate.

## Concurrency

- Exclusive Continue of a run still requires writer transfer.
- A shared board is different: one Origin serializes members' operations with
  CAS. A conflicting write is refused and retried after reload.

## Export and checkpoint

- Being able to read a shared board is not the same as being allowed to copy it
  out. Export/checkpoint must decide, per permission, between preserving a
  reference and embedding a snapshot; a shared team Resource defaults to a
  reference.

## Detected mismatches

- `data_resources` CHECK constraints cannot be altered in SQLite; P1 needs a
  table rebuild rather than an `ALTER TABLE`.
- `data_resource_grants.subject_kind` allows only `user`/`instance`; P2 avoids
  another rebuild by adding a separate organization-grant table, but the two
  grant sources must share the epoch/revocation checks.
