---
title: "ADR-012: Capsule lifecycle vs revision lifecycle (two records)"
status: draft          # draft | accepted | archived
date: 2026-07-13
author: "@egamikohsuke"
related:
  - "GITHUB_CAPSULE_REQUEST_PIPELINE.md"
  - "EXECUTION_IDENTITY_SPEC.md"
---

# ADR-012: Capsule lifecycle vs revision lifecycle (two records)

> Cross-repo note: the columns and their enforcement live in **ato-api**. This
> ADR is recorded in the ato repo for cross-repo visibility because the
> [GitHub Capsule Request Pipeline](GITHUB_CAPSULE_REQUEST_PIPELINE.md) depends
> on it: a capsule's *existence* is gated by capsule `lifecycle`, while a
> *candidate/revision* moves through its own lifecycle before it becomes the
> capsule's current published revision.

## Context

A GitHub-request capsule is not born published: the pipeline materializes,
generates, validates, and QAs a candidate, and only then may it be published. A
capsule may later be taken down. But there is a second, sharper requirement:
**an update to an already-published capsule must never un-publish the current
revision.** Verification of the new source happens on a *new* revision; the live
capsule keeps serving its current revision until the new one is fully published.

Today "is this capsule live and to whom" is spread across several columns —
`visibility`, `playground_status`, `verification_state`, `content_quality_state`
— enforced in more than one place. Overloading `visibility` (an *audience*
concept) to also mean "does this capsule exist yet" (a *lifecycle* concept), and
folding per-revision verification state onto the capsule itself, are the two
root confusions.

## Decision

Split lifecycle into **two records**, orthogonal to `visibility`:

### 1. Capsule lifecycle — existence / takedown gate

Carried on the **capsule** row:

```text
capsule.lifecycle = active | archived | blocked
```

- `active` — the capsule exists and may be served (subject to having a published
  current revision and to `visibility`).
- `blocked` — administratively withheld from existence (policy).
- `archived` — taken down; removed from existence regardless of any revision.

### 2. Revision lifecycle — per-candidate/revision progression

Carried on a **per-revision record** (`capsule_revisions`), **not** on the
capsule:

```text
revision.lifecycle = draft | verifying | rejected
                   | publish_ready | awaiting_admin_approval | published
```

The capsule holds a **`current_revision_id`** pointer. Verification runs on the
*new* revision; when that revision reaches `published`, the pointer switches to
it **atomically**. The previously-current revision is untouched until that
switch, so a published capsule is never un-published by an in-flight update.

### Precedence (evaluated in this order)

1. **Capsule `lifecycle`** — the existence gate. Not `active` ⇒ not
   runnable/listable, regardless of everything else.
2. **Current revision `lifecycle`** — the capsule is servable only if
   `current_revision_id` points to a `published` revision.
3. **`visibility`** — the audience gate, applied only once (1) and (2) admit.
4. `playground_status` / `verification_state` / `content_quality_state` —
   **presentation** only (badges, ordering, surfacing), never existence.

**The resolve/run gate is therefore: capsule `active` AND current revision
`published`.** Every place that previously said "`lifecycle = published` gates
resolve/run" now means exactly this pair.

**Enforcement in ONE place.** After a centralization refactor, the
capsule-lifecycle → current-revision → visibility → presentation precedence is
enforced in a single code path, so no surface can accidentally list or run a
capsule that is not `active` with a `published` current revision.

**Archived cutoff for the app proxy.** Archiving a capsule (e.g. takedown) sets
capsule `lifecycle = archived` and **revokes all its run bindings in the takedown
D1 batch**. The app proxy gates **per-run**, so once bindings are revoked the
capsule stops being runnable at the next run attempt — no separate proxy purge
is needed.

## Alternatives Considered

### Option A: One `lifecycle` column on the capsule (the earlier draft)

- Pro: one column, one migration.
- Con: cannot express "publish a new revision without disturbing the current
  one" — verification state on the capsule would flip a live capsule to
  `verifying`. It conflates the capsule's existence with a candidate's
  progression. Rejected: this is exactly the un-publish-on-update hazard.

### Option B: Keep overloading `visibility`

- Pro: no migration.
- Con: conflates existence with audience; the source of the current duplicated,
  divergent gating. Rejected.

### Option C: A boolean `is_published` on the capsule

- Pro: minimal.
- Con: cannot express `verifying` / `rejected` / `awaiting_admin_approval` per
  revision, nor `blocked` / `archived` existence. Rejected.

## Consequences

- **Good**: publishing an update is safe — the live revision serves until the new
  revision is `published`, then the pointer switches atomically.
- **Good**: one clear existence gate on the capsule; `visibility` regains a
  single meaning; presentation columns stop being load-bearing for access
  control.
- **Good**: the pipeline maps cleanly — the candidate's `pipeline_state` tail
  (`publish_ready → awaiting_admin_approval → published`) is mirrored on the
  revision record; `blocked`/`archived` live on the capsule.
- **Bad / risk**: two records and a pointer are more moving parts than one
  column, and gating now reads two rows. The staged migration below de-risks the
  flip.

### Migration (staged, reversible until the last step)

1. **Nullable columns** — add capsule `lifecycle` and the `capsule_revisions`
   table + `current_revision_id`, all nullable; nothing enforces them yet.
2. **Production inventory** — classify every existing capsule's true state and
   its current revision.
3. **Explicit published-ID-list artifact** — freeze the set of capsule IDs that
   are published *today* as an out-of-band artifact. In the two-record model a
   "published today" capsule maps to **capsule `active` AND a `published` current
   revision**.
4. **Dual-read diff gate** — run new gating in shadow and require
   `(active ∧ current revision published) count == current store-public count`
   (and the ID sets match the frozen list) before trusting it.
5. **Enforcement flag** — only after the diff gate is clean, flip enforcement on.

## Follow-up

- The centralization refactor (single enforcement path) is a prerequisite for
  step 5.
- The atomic `current_revision_id` switch must be part of the same D1 batch that
  flips the new revision to `published`.
- Coordinate the takedown D1 batch (binding revocation) with the app proxy's
  per-run gating so archived capsules stop at the next run.
