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
- Fetch the **GitHub Trees listing, recursive-first** (one API call for the
  whole tree). If GitHub returns `truncated=true`, the repo is too large to
  enumerate reliably → route to `blocked_repo`.
- Hand the pinned commit and the Trees listing (path set + blob OIDs) to the
  builder job as inputs. **No repository content is materialized on the
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

### Why recursive-first Trees listing

- The MVP file cap is **50,000 files**. GitHub's Trees API truncates at roughly
  **100,000 entries / 7 MB** of response. So the entire admissible range sits
  *under* the truncation threshold: a single recursive call enumerates any repo
  we would accept, and `truncated=true` is itself a clean, cheap signal that the
  repo is out of range (`blocked_repo`).
- A **non-recursive** walk costs one API call *per directory*, which is both
  slower and a rate-limit risk, for no benefit inside our size envelope.

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
- Con: loses the independent cross-check. Having the Worker fetch the
  authoritative Trees listing and the builder verify the checkout against it
  gives a cheap integrity gate (path set + OIDs) and a fast pre-checkout
  `truncated` rejection. Rejected.

## Consequences

- **Good**: heavy work runs where heavy work belongs; the Worker stays within
  its limits; the builder reuses `materialize_source` and its lease/receipt
  machinery instead of a parallel implementation.
- **Good**: the recursive Trees call doubles as the oversize gate, so oversize
  repos are rejected before any checkout.
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
