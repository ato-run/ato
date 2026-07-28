---
title: "Source Materialization Spec: the source_materialize builder job"
status: draft          # draft | accepted | archived
date: 2026-07-13
author: "@egamikohsuke"
ssot:
  - "crates/snapshot/src/rootfs_builder.rs"
related:
  - "ADR-011-source-materialization-placement.md"
  - "A1_SOURCE_TREE_PROFILE.md"
  - "GITHUB_CAPSULE_REQUEST_PIPELINE.md"
  - "HASH_AND_PROVENANCE_POLICY.md"
---

# Source Materialization Spec

## 1. Overview

`source_materialize` is a snapshot-builder job that turns a public GitHub
repository at a pinned commit into a frozen, content-addressed source archive.
It is the first builder step of the
[GitHub Capsule Request Pipeline](GITHUB_CAPSULE_REQUEST_PIPELINE.md). The
placement decision (why the builder, not the Worker) is
[ADR-011](ADR-011-source-materialization-placement.md); this spec defines the
job's contract.

## 2. Scope

### In scope

- The `source_materialize` job kind: inputs, steps, outputs, caps, failure
  modes.
- The archive format, its `source_archive_hash`, and its R2 key.
- Retention and GC of `blocked_*` archives.

### Out of scope

- The hash algorithm itself ([A1_SOURCE_TREE_PROFILE.md](A1_SOURCE_TREE_PROFILE.md)).
- Ref resolution and Trees listing on the Worker ([ADR-011](ADR-011-source-materialization-placement.md)).
- QA of the candidate capsule (the `candidate_qa` job; see the pipeline spec §4.6).

## 3. Design

### 3.1 Job kind on the existing lane

`source_materialize` is a **new job kind on the existing snapshot-builder
claim/ack lease lane** — the same lease/ack/receipt machinery snapshot builds
already use. It is not a new service and not a new lane.

### 3.2 Inputs

Provided by ato-api when the candidate enters `fetching`:

- `candidate_id`, `attempt_no`.
- `source_provider = github`, `provider_repository_id`, `provider_owner`,
  `provider_repo`.
- `commit_algorithm`, `commit_oid` (the pinned immutable commit).
- The **GitHub Trees listing** (assembled recursive-first with a non-recursive
  fallback when the recursive response is truncated — see
  [ADR-011](ADR-011-source-materialization-placement.md)): the authoritative path
  set and blob OIDs.
- A short-lived **GitHub App installation token**, scoped to **public reads
  only**.

### 3.3 Steps

1. **Checkout** the pinned `commit_oid`, reusing/extending `materialize_source`
   (`crates/snapshot/src/rootfs_builder.rs`). Checkout is by commit OID only —
   never a branch or tag.
2. **Cross-check** the checked-out working tree against the API-provided Trees
   listing: the path set and blob OIDs must match. A mismatch (repo mutated
   between listing and checkout, or content present that the listing did not
   describe) fails the job.
3. **Enforce the A1v2 admissibility profile** (see the source-tree profile
   §3.3): reject symlinks, submodules, LFS pointers, non-UTF-8/non-NFC paths,
   case-fold collisions, and unsupported node types. A violation maps to
   `blocked_repo` / `blocked_incompatible`.
4. **Canonicalize → hash**: compute `materialized_source_tree_hash` (A1v2, the
   A1 `sha256` digest of the admissible tree).
5. **Archive**: write a deterministic `tar.zst`; compute
   `source_archive_hash = sha256(exact tar.zst bytes)`.
6. **Upload (API-mediated)**: the builder asks ato-api to authorize the write
   and receives a short-lived presigned `PUT` for exactly one object key. The
   builder never holds R2 credentials and never names the object: ato-api
   derives the key from the archive digest.

   ```text
   source-archives/{algorithm}/{digest}
   ```

   This is the implemented and authoritative form — `object_key_for_archive`
   (Rust), `expectedObjectKey` (TypeScript), and the
   `source_materializations_object_key_ck` CHECK in ato-api drizzle/0131 all
   derive it identically, so a locator that disagrees with its content address
   is unwritable rather than merely discouraged.

   It replaces the earlier draft form
   `source-archives/v1/sha256/{source_archive_hash}.tar.zst`, for two reasons.
   The `v1` and the `.tar.zst` suffix both encode the archive FORMAT into the
   locator, and the format already has a home — `archive_format_version` in the
   materialization receipt — so carrying it twice invites the two to disagree.
   And pinning `sha256` in the path makes the key un-derivable the day the
   digest algorithm moves, whereas `{algorithm}` carries it.

   The colon of `<algorithm>:<hex>` is split rather than kept, because object
   stores differ on whether a colon in a key is legal and a key that works on
   one and not another is a portability problem discovered at the worst time.
7. **Emit a receipt** recording commit OID, both hashes, caps observed, and the
   final state, and advance the candidate to `analyzing`.

### 3.3.1 What is identity, and what is not

The two are kept apart deliberately, because collapsing them makes a re-encoding
of the same source look like different source.

```text
source_tree_digest      = source IDENTITY
                          what the source IS; immutable

object key              = derived from the digest of the SERIALIZED ARCHIVE
                          bytes; a locator, not an identity

source_archive_digest   = the archive BYTES; one encoding of the source

archive_format_version  = carried in the materialization receipt, and in the
                          uniqueness key of `source_materializations`
```

Neither the object key nor the archive digest is part of a Source Revision's
identity. A Source Revision is keyed on six elements — provider, canonical
repository, commit algorithm, resolved commit sha, `source_tree_digest`,
resolver contract version — and none of them describe how the bytes were packed.

That is what allows one revision to have more than one materialization: a new
archive format is a second legitimate encoding of the *same* source, recorded as
a new row under a new `archive_format_version`, never as a new identity and never
as an update to the immutable one. Within a single format archiving is
deterministic, so two materializations that disagree are contradictory evidence
rather than a duplicate, and are refused.

### 3.4 Caps

Enforced during checkout/walk; exceeding any cap fails the job (routes to
`blocked_repo`):

| Cap | Limit |
|-----|-------|
| Compressed archive size | 100 MiB |
| Expanded tree size | 250 MiB |
| File count | 50,000 files |
| Single file size | 50 MiB |

These bound builder resource use against a hostile or merely huge repo. The
**50,000-file** cap is the enumeration limit the Worker enforces while assembling
the Trees listing (recursive-first + non-recursive fallback under an API-call
budget); a recursive listing that merely truncates is *not* a block on its own
(see [ADR-011](ADR-011-source-materialization-placement.md)).

### 3.5 The frozen archive is the only QA input

Downstream stages — analysis, generation, and `candidate_qa` — consume **only
the frozen archive**, never a live re-clone. This guarantees every later stage
sees byte-identical source and that `materialized_source_tree_hash` /
`source_archive_hash` describe exactly what was analyzed and verified.

### 3.6 Retention and GC

- A `blocked_*` candidate's archive (if one was produced before the block) is
  **retained 30 days**, then **GC'd if unreferenced**.
- **Hashes, provenance, and receipts are kept for audit** regardless of archive
  GC — so a blocked decision remains explainable after its bytes are collected.
- A published capsule's archive is referenced and is not GC'd while referenced.

## 4. Interface

### 4.1 Outputs (recorded on the candidate)

- `materialized_source_tree_hash` — A1v2 `sha256` identity.
- `source_archive_hash` — `sha256` of the `tar.zst` bytes.
- R2 object at `source-archives/v1/sha256/{source_archive_hash}.tar.zst`.
- A materialize receipt (commit OID, hashes, caps, state).

### 4.2 Failure mapping

| Condition | Pipeline result |
|-----------|-----------------|
| >50,000 files, or Trees enumeration API-call budget exceeded (Worker) | `blocked_repo` |
| Cap exceeded | `blocked_repo` |
| A1v2 admissibility violation | `blocked_repo` / `blocked_incompatible` |
| Checkout ↔ listing mismatch | `failed_internal` (retryable, max 3) |
| Token/transient IO failure | `failed_internal` (retryable, max 3) |

## 5. Security

- **Public repos only**, via a scoped GitHub App installation token; no private
  content is fetched.
- **API-mediated R2 write** keeps R2 credentials off the builder.
- The caps and the checkout-by-OID rule bound resource use and pin identity so a
  repo cannot swap content under the pipeline.

## 6. Known limitations

- No LFS resolution and no symlink handling in the MVP — such repos are blocked
  rather than materialized.
- Idempotent upload by content hash depends on the R2 conditional-put question
  (pipeline spec §8): until resolved, a re-run may re-upload identical bytes to
  the same key.

## References

- `crates/snapshot/src/rootfs_builder.rs:1474` — `materialize_source` (reused/extended).
- [ADR-011-source-materialization-placement.md](ADR-011-source-materialization-placement.md) — the placement decision.
- [A1_SOURCE_TREE_PROFILE.md](A1_SOURCE_TREE_PROFILE.md) — the hash and admissibility rules.
- [HASH_AND_PROVENANCE_POLICY.md](HASH_AND_PROVENANCE_POLICY.md) — hash domains.
