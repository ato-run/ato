---
title: "A1v2 Source-Tree Profile: deterministic identity for materialized repository source"
status: draft          # draft | accepted | archived
date: 2026-07-13
updated: 2026-07-30
author: "@egamikohsuke"
ssot:
  - "crates/capsule/src/foundation/blob/tree_hash.rs"
related:
  - "../accepted/A1_BLOB_HASH.md"
  - "HASH_AND_PROVENANCE_POLICY.md"
  - "SOURCE_MATERIALIZATION_SPEC.md"
  - "GITHUB_CAPSULE_REQUEST_PIPELINE.md"
---

# A1v2 Source-Tree Profile

## 1. Overview

`materialized_source_tree_hash` is the deterministic, content-addressable
identity of a repository checkout after it has been materialized and
canonicalized. It is what the [GitHub Capsule Request Pipeline](GITHUB_CAPSULE_REQUEST_PIPELINE.md)
uses to prove that "this exact source tree" is what was analyzed, verified, and
published.

This document defines the **A1v2 source-tree profile**: a set of admissibility
preconditions layered on top of the frozen [A1 blob-hash algorithm](../accepted/A1_BLOB_HASH.md).
A1v2 is the second profile in the A1 hashing family. It does **not** change the
A1 v1 digest — it constrains which trees are *allowed* to be hashed as source.

## 2. Scope

### In scope

- The preconditions a materialized checkout must satisfy to have a
  `materialized_source_tree_hash`.
- The definition of `materialized_source_tree_hash` in terms of the A1 v1
  algorithm.
- The repository-internal symlink rules and the byte-level
  `source_archive_hash`.

### Out of scope

- Where materialization runs and how the archive is stored (see
  [SOURCE_MATERIALIZATION_SPEC.md](SOURCE_MATERIALIZATION_SPEC.md)).
- Dependency-blob hashing and the derivation cache (unchanged; see
  [A1_BLOB_HASH.md](../accepted/A1_BLOB_HASH.md)).
- Submodule or LFS resolution.

## 3. Design

### 3.1 Relationship to frozen A1 (the decision)

A1 v1 is **frozen**: once a blob is published with `blob_hash`, the algorithm
must not change. A1v2 respects that freeze completely.

**Decision:** a source tree with no symlink reuses the A1 v1 per-node digests
and top-level fold **verbatim** — the same code
(`crates/capsule/src/foundation/blob/tree_hash.rs`), the same
`ato-blob-v1` prefix, and the same `sha256:<hex>` wire value. This preserves
every existing source identity byte-for-byte.

For a tree containing an admitted symlink, A1v2 applies a domain-separated
extension over the frozen A1 digest and the validated link records:

```text
regular tree:
  materialized_source_tree_hash = A1_blob_hash(tree)

tree containing symlinks:
  materialized_source_tree_hash =
    sha256(
      "ato-source-tree-symlink-v1\0" ||
      field(A1_blob_hash(tree)) ||
      for each symlink in deterministic A1 walk order:
        "symlink\0" ||
        field(repository_relative_link_path) ||
        field(raw_target_utf8) ||
        field(normalized_repository_relative_resolved_target) ||
        field(target_kind) # "file" or "dir"
    )
```

`field(x)` is the unsigned 64-bit big-endian byte length followed by `x`.
The extension is necessary because the frozen A1 link node commits the raw
target but not its normalized resolved target or terminal kind. A link is
never flattened into the target bytes.

Rationale for reusing the digest rather than minting a new prefix:

- **Reuse over new logic.** The digest, the byte-ordering, the executable-bit
  rule, and the empty-directory exclusion are already correct and battle-tested
  in `tree_hash.rs`. A source-tree profile needs none of them changed.
- **Existing A1 outputs are unaffected by construction** — the v1 algorithm is
  not touched, so every already-published `blob_hash` verifies identically.
- Domain separation is provided by *context*, not by the hash: a
  `materialized_source_tree_hash` is always stored and transmitted in a
  source-provenance field (per [HASH_AND_PROVENANCE_POLICY.md](HASH_AND_PROVENANCE_POLICY.md)
  §2), never as a `store/blobs/` identity.

This closes three open decisions from HASH_AND_PROVENANCE_POLICY §12:
`sha256` only for identity (blake3 is transport-only, §3.4); the exact binary
encoding is A1's per-node scheme; and `source_tree_hash` is **mandatory** for
GitHub-request capsules.

### 3.2 Inherited A1 v1 properties (unchanged)

The following come from A1 v1 and are restated here only for readers:

- Per-entry digests: `file:` includes `basename`, the `S_IXUSR` executable
  mode byte, and the content SHA-256; `dir:` folds sorted child hashes.
- **Git executable bit**: `mode_byte = 1` iff `S_IXUSR` (`0o100`) is set,
  else `0`. This is the only permission bit that participates.
- Children are sorted **lexicographically by raw basename bytes**.
- Recursively empty directories are excluded.
- `mtime`/`atime`/`ctime`, uid/gid, xattrs, and ACLs are ignored.

### 3.3 A1v2 admissibility profile (the preconditions)

A materialized checkout is **admissible as source** only if it satisfies all of
the following. A tree that violates any rule has **no**
`materialized_source_tree_hash`; the pipeline routes it to `blocked_repo` /
`blocked_incompatible` (see the pipeline spec §4.2).

1. **Paths are UTF-8.** Any non-UTF-8 path component → reject.
2. **Paths are Unicode NFC.** Every path component must already be in
   Normalization Form C. A non-NFC component → reject. (Because NFC is required,
   the raw basename bytes A1 sorts on *are* the NFC bytes, so A1's byte ordering
   is deterministic and stable across platforms with no additional normalization
   step at hash time.)
3. **No Unicode case-fold collisions.** Within any single directory, if two
   entry names are distinct but fold to the same value under Unicode simple
   case-folding, the tree is rejected. This prevents a tree that is valid on a
   case-sensitive filesystem from silently colliding on a case-insensitive one.
4. **Only closed repository-internal symlinks.** A link is admitted only when:
   - its raw target is non-empty UTF-8 without NUL, is relative, and uses `/`
     separators;
   - it has no absolute root, Windows drive prefix, UNC/backslash form, empty
     component, or trailing separator;
   - resolving it relative to the link's parent stays within the repository;
   - the entire link chain resolves to an existing regular file or directory;
   - the chain contains no self-cycle, multi-node cycle, or directory recursion;
   - resolution stays within 40 link expansions per chain and 100,000 total
     expansions per tree;
   - each path component is at most 255 UTF-8 bytes, each raw target at most
     4,096 bytes, and each normalized resolved path at most 4,096 bytes.
   Absolute links, escapes, dangling links, cycles, depth/expansion overflow,
   ambiguous cross-platform spelling, and links that traverse a regular file
   are typed `blocked_repo` failures.
5. **No submodules.** A git submodule (gitlink entry) → reject.
6. **No LFS pointers.** A Git-LFS pointer file (unresolved `version
   https://git-lfs...` stub) → reject. The MVP does not resolve LFS.
7. **No unsupported node types.** Device files, sockets, and FIFOs are rejected
   (already an error in A1).

Except for the explicit symlink extension in §3.1, these are admissibility
rules rather than hash inputs. In particular, lifting the blanket link refusal
does not re-identify any already-admissible regular tree.

### 3.3.1 One validator at every boundary

`source_tree::validate_source_tree` is the single normalizer and resolver used
by the checkout walk and deterministic archive writer. The archive extractor
first admits only `Normal` entry paths and regular/directory/symlink entry
kinds, delays creation of links until all content writes are complete, then
re-runs the same whole-tree validator. Program-source staging copies validated
links without dereferencing and validates the result again.

This ordering closes the archive-link traversal class: no archive entry can
redirect a later write outside the extraction root, and a link whose meaning
would differ after extraction is rejected instead of normalized silently.

### 3.3.2 Security rationale

Repository-internal links are useful source structure, but following arbitrary
filesystem links would cross Ato's source boundary. The policy therefore
commits link structure to identity, resolves lexically from the repository
root, never calls filesystem canonicalization through a link, and serializes
the link itself. Explicit depth, expansion, entry, and byte limits bound
adversarial chains. The result widens compatibility without widening the
filesystem authority of the builder or runtime.

### 3.4 `source_archive_hash` (byte identity)

Separately from the tree-content identity, the frozen archive has a byte
identity:

```text
source_archive_hash = sha256(exact tar.zst bytes)
```

- It is the hash of the **exact bytes** of the `.tar.zst` the builder produced —
  not of the tree content. Two different valid `tar.zst` encodings of the same
  tree would have the same `materialized_source_tree_hash` but different
  `source_archive_hash`.
- Canonical R2 key: `source-archives/v1/sha256/{source_archive_hash}.tar.zst`
  (storage mechanics in [SOURCE_MATERIALIZATION_SPEC.md](SOURCE_MATERIALIZATION_SPEC.md)).
- `materialized_source_tree_hash` is the identity QA and publication reason
  about; `source_archive_hash` is the transport/storage integrity of the frozen
  bytes.

### 3.5 Hash-role split

| Role | Algorithm | Used for |
|------|-----------|----------|
| Identity | `sha256` | A1 family: `materialized_source_tree_hash`, `source_archive_hash`, derivation/blob hashes |
| Transport | `blake3` | CapsuleFS CAS chunk addressing and the structural-id family (`ReadyStateManifest::id()`) |

`sha256` and `blake3` are never substituted for one another.

## 4. Golden vectors (required)

Both repos' CI must golden-test:

1. **A1 v1 regression** — the existing `blob_hash` golden vectors must be
   byte-for-byte unchanged (proves the freeze held).
2. **A1v2 conformant** — a fixed conformant source tree hashes to a pinned
   `materialized_source_tree_hash`.
3. **A1v2 symlink acceptance** — relative file, directory, nested-chain, and
   JS Paint `tracky-mouse` fixtures, including archive round-trip identity.
4. **A1v2 rejection** — non-UTF-8/NFC/case-collision paths; absolute, escaping,
   dangling, cyclic, over-depth, over-length, or ambiguous links; submodule,
   LFS pointer, and device node.
5. **Identity separation** — raw target spelling changes the source identity,
   and a regular file never shares identity with a symlink to identical bytes.

## 5. Known limitations

- Submodules and LFS remain unsupported; a repo that needs them is
  `blocked_repo`.
- NFC is required, not applied: the materializer rejects non-NFC paths rather
  than normalizing them, so that identity is never silently changed by the
  platform.

## References

- [../accepted/A1_BLOB_HASH.md](../accepted/A1_BLOB_HASH.md) — the frozen A1 v1 algorithm.
- `crates/capsule/src/foundation/blob/tree_hash.rs` — reference implementation of A1 v1 (reused verbatim).
- [HASH_AND_PROVENANCE_POLICY.md](HASH_AND_PROVENANCE_POLICY.md) §2, §5, §12 — hash domains and the open decisions this profile closes.
- [SOURCE_MATERIALIZATION_SPEC.md](SOURCE_MATERIALIZATION_SPEC.md) — archive storage and the materialize job.
