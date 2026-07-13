---
title: "ADR-012: Capsule lifecycle column, orthogonal to visibility"
status: draft          # draft | accepted | archived
date: 2026-07-13
author: "@egamikohsuke"
related:
  - "GITHUB_CAPSULE_REQUEST_PIPELINE.md"
  - "EXECUTION_IDENTITY_SPEC.md"
---

# ADR-012: Capsule lifecycle column, orthogonal to visibility

> Cross-repo note: the column and its enforcement live in **ato-api**. This ADR
> is recorded in the ato repo for cross-repo visibility because the
> [GitHub Capsule Request Pipeline](GITHUB_CAPSULE_REQUEST_PIPELINE.md) depends
> on it (a request is `draft` → `verifying` → `published`, and takedown →
> `archived`).

## Context

A GitHub-request capsule is not born published: it exists as a draft while the
pipeline materializes, generates, validates, and QAs it, becomes `verifying`
during QA, and only becomes `published` on success. It may be `blocked` by
policy or `archived` by takedown.

Today, "is this capsule live and to whom" is spread across several columns —
`visibility`, `playground_status`, `verification_state`, `content_quality_state`
— and the gating logic is enforced in more than one place. Overloading
`visibility` (an *audience* concept) to also mean "does this capsule exist yet"
(a *lifecycle* concept) has caused confusion and duplicated checks.

## Decision

Introduce a dedicated **`lifecycle`** column, **orthogonal to `visibility`**:

```text
lifecycle = draft | verifying | published | blocked | archived
```

- `visibility` continues to mean *audience* (who may see a live capsule).
- `lifecycle` means *existence* (whether the capsule is live at all).

**Precedence (evaluated in this order):**

1. `lifecycle` — the **existence gate**. Not `published` ⇒ not runnable/listable,
   regardless of everything else.
2. `visibility` — the **audience** gate, applied only once lifecycle admits.
3. `playground_status` / `verification_state` / `content_quality_state` —
   **presentation** only (badges, ordering, surfacing), never existence.

**Enforcement in ONE place.** After a centralization refactor, the
lifecycle→visibility→presentation precedence is enforced in a single code path,
so no surface can accidentally list or run a non-`published` capsule.

**Archived cutoff for the app proxy.** Archiving a capsule (e.g. takedown)
**revokes all its run bindings in the takedown D1 batch**. The app proxy gates
**per-run**, so once bindings are revoked the capsule stops being runnable at the
next run attempt — no separate proxy purge is needed.

## Alternatives Considered

### Option A: Keep overloading `visibility`

- Pro: no migration.
- Con: conflates existence with audience; the source of the current duplicated,
  divergent gating. Rejected.

### Option B: A boolean `is_published`

- Pro: minimal.
- Con: cannot express `verifying`, `blocked`, or `archived` — exactly the states
  the pipeline needs to distinguish (in-flight vs blocked vs taken-down).
  Rejected.

## Consequences

- **Good**: one clear existence gate; `visibility` regains a single meaning;
  presentation columns stop being load-bearing for access control.
- **Good**: the pipeline maps cleanly onto lifecycle (`draft` → `verifying` →
  `published`; `blocked`; `archived`).
- **Bad / risk**: it is a new gating column on a live inventory. A wrong default
  or a premature enforcement flip could hide currently-public capsules. Hence
  the staged migration below.

### Migration (staged, reversible until the last step)

1. **Nullable column** — add `lifecycle` nullable; nothing enforces it yet.
2. **Production inventory** — classify every existing capsule's true state.
3. **Explicit published-ID-list artifact** — freeze the set of IDs that are
   published *today* as an out-of-band artifact.
4. **Dual-read diff gate** — run new gating in shadow and require
   `published count == current store-public count` (and the ID sets match the
   frozen list) before trusting it.
5. **Enforcement flag** — only after the diff gate is clean, flip enforcement on.

## Follow-up

- The centralization refactor (single enforcement path) is a prerequisite for
  step 5.
- Coordinate the takedown D1 batch (binding revocation) with the app proxy's
  per-run gating so archived capsules stop at the next run.
