---
title: "ADR-011: Source materialization runs on the builder, not the Worker"
status: draft          # draft | accepted | archived
date: 2026-07-13
author: "@egamikohsuke"
related:
  - "GITHUB_CAPSULE_REQUEST_PIPELINE.md"
  - "SOURCE_MATERIALIZATION_SPEC.md"
  - "A1_SOURCE_TREE_PROFILE.md"
---

# ADR-011: Source materialization runs on the builder, not the Worker

## Context

The [GitHub Capsule Request Pipeline](GITHUB_CAPSULE_REQUEST_PIPELINE.md) must
turn a public GitHub repository at a pinned commit into a canonicalized,
content-addressed source archive. That work — clone/checkout, walk the tree,
enforce the [A1v2 admissibility profile](A1_SOURCE_TREE_PROFILE.md), hash,
compress — is CPU-, memory-, and IO-bound and unbounded in the general case.

The platform's request intake is a Cloudflare Worker (ato-api edge). Workers
have hard CPU-time and memory limits and no persistent local filesystem, so they
are the wrong place to expand a repository. But the platform already has a
component built for heavy, leased, receipted work: the **snapshot builder**,
which runs on real hosts and already checks out source via
`materialize_source` in `crates/snapshot/src/rootfs_builder.rs`.

We must decide which surface does what.

## Decision

Materialization is **split** across the Worker and the builder, with the heavy
work on the builder:

**ato-api (Worker) does metadata only:**

- Resolve the requested ref (branch/tag/URL) to an **immutable commit OID** and
  pin it. Mutable refs are never carried forward as identity (consistent with
  [HASH_AND_PROVENANCE_POLICY.md](HASH_AND_PROVENANCE_POLICY.md) §3.2).
- **Enumerate the tree, recursive-first with a non-recursive fallback** (see the
  normative flow below), assembling the authoritative path set + blob OIDs
  within an explicit API-call budget.
- Hand the pinned commit and the assembled Trees listing (path set + blob OIDs)
  to the builder job as inputs. **No repository content is materialized on the
  Worker.**

**Builder does the content work** via a new job kind `source_materialize` on the
**existing snapshot-builder claim/ack lease lane**:

- Pinned checkout of the commit, reusing/extending `materialize_source`.
- **Cross-check** the checked-out tree against the API-provided Trees listing
  (the path set and blob OIDs must match) — this detects a repo that changed
  between listing and checkout, and any content the checkout produced that the
  authoritative listing did not describe.
- Canonicalize → A1v2 hash → archive (`tar.zst`) → **API-mediated upload to R2**.

Access uses a **GitHub App installation token**, scoped to **public repos
only**.

### Tree enumeration flow (normative)

The earlier draft claimed a recursive call always fits under GitHub's truncation
threshold for repos below the 50,000-file cap. **That is wrong.** GitHub's
recursive Trees response has a **~7 MB response-size cap** in addition to the
entry-count cap, so `truncated=true` can occur **well below 50,000 entries** (a
repo with long paths hits the byte cap first). GitHub's own docs recommend
falling back to non-recursive traversal when a recursive listing is truncated.

Normative enumeration:

1. **Recursive-first.** One recursive Trees call. If `truncated=false`, that is
   the authoritative listing — done.
2. **Non-recursive fallback.** If `truncated=true`, walk the tree
   **non-recursively, one call per directory**, accumulating the path set + blob
   OIDs, **under an explicit API-call budget**.
3. **Block only on real limits.** Route to `blocked_repo` **only** when the file
   count exceeds **50,000** or the **API-call budget is exceeded** — never merely
   because the first recursive call was truncated.

- A non-recursive walk costs one API call per directory, so the API-call budget
  (not `truncated` itself) is the guard against pathological directory fan-out.

## Alternatives Considered

### Option A: Materialize on the Worker

- Pro: one fewer hop; no lease handoff.
- Con: Workers cannot expand an arbitrary repo — CPU/memory/wall-time limits and
  no persistent FS. A hostile or merely large repo trivially exceeds limits.
  Rejected.

### Option B: A brand-new dedicated materialization service

- Pro: clean separation of concerns.
- Con: duplicates the builder's leasing, receipting, host pool, and the existing
  `materialize_source` checkout logic — exactly the parallel-copy the workspace
  rules forbid. Rejected in favor of a new **job kind** on the existing lane.

### Option C: Builder fetches the tree itself, no API Trees listing

- Pro: one authority.
- Con: loses the independent cross-check. Having the Worker assemble the
  authoritative Trees listing and the builder verify the checkout against it
  gives a cheap integrity gate (path set + OIDs) and a fast pre-checkout oversize
  rejection (file-count / API-call budget). Rejected.

## Consequences

- **Good**: heavy work runs where heavy work belongs; the Worker stays within
  its limits; the builder reuses `materialize_source` and its lease/receipt
  machinery instead of a parallel implementation.
- **Good**: tree enumeration doubles as the oversize gate (file-count / API-call
  budget), so oversize repos are rejected before any checkout — while a merely
  truncated recursive listing is handled by the non-recursive fallback, not
  mistaken for oversize.
- **Bad / cost**: one lease handoff (Worker → builder) is added to the critical
  path, and the builder must reconcile its checkout against the API listing.
  This is the price of keeping materialization off the Worker.
- **Bad**: a repo that mutates between listing and checkout is detected but
  fails the job (retried up to the `failed_internal` limit) rather than silently
  proceeding — correct, but a source of transient failures for very active
  repos.

## Follow-up

- Contract details (caps, archive format, R2 keys, retention/GC) live in
  [SOURCE_MATERIALIZATION_SPEC.md](SOURCE_MATERIALIZATION_SPEC.md).
- The R2 conditional-put question (idempotent upload by content hash) is an
  open item in the pipeline spec §8.
