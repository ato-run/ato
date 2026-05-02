---
title: "A1 Blob Hash: deterministic content-addressable tree hashing"
status: accepted
date: 2026-05-03
author: "@egamikohsuke"
related:
  - docs/rfcs/draft/A1_PROJECTION_STRATEGY.md
  - docs/rfcs/draft/A1_GC.md
---

# A1 Blob Hash

## Status

**Accepted, frozen.** The algorithm described below is the wire format of
`blob_hash` for the A1 derivation cache. Once any blob is published with
this hash, **the algorithm must not change**. A future revision must ship
under a new prefix (`ato-blob-v2`, …) and live alongside `v1` until all
v1 blobs are evicted.

## Context

A1 introduces a whole-tree derivation cache: a successful dependency
install is frozen into an immutable directory under `~/.ato/store/blobs/`
and re-projected into future runs. The cache key has two parts:

- `derivation_hash` — identifies the *recipe* (lockfile, ecosystem,
  policies). Computed from `DepDerivationKeyV1` via JCS + SHA-256.
- `blob_hash` — identifies the *frozen output* of running that recipe.

`blob_hash` must be:

1. **Deterministic across machines**: identical content must produce the
   same hash regardless of mtime, owner, umask, or where the tree is
   stored on disk.
2. **Stable across Ato versions**: a blob frozen by `ato 0.5` must verify
   under `ato 0.6` without re-walking the tree.
3. **Content-addressable**: any single byte of payload change must change
   the hash.
4. **Inexpensive to verify**: a single SHA-256 sweep over the tree is
   acceptable; multi-pass canonicalization is not.

## Decision

`blob_hash` is computed by recursively hashing entries inside the root
directory and folding the result under a versioned prefix.

### Per-entry digests

For every entry inside a directory we compute a 32-byte SHA-256:

```text
file:    sha256(b"file\0" || basename || b"\0" || mode_byte || b"\0" || content_sha256)
dir:     sha256(b"dir\0"  || basename || b"\0" || concat(sorted_child_hashes))
symlink: sha256(b"link\0" || basename || b"\0" || link_target_bytes)
```

Where:

- `basename` is the entry's file name as raw bytes. On Unix this is
  `OsStr::as_bytes()`. **No Unicode normalization is applied.**
- `mode_byte` is `1u8` if the regular file's owner-executable bit
  (`S_IXUSR`, `0o100`) is set, otherwise `0u8`.
- `content_sha256` is the 32-byte SHA-256 of the file's bytes.
- `link_target_bytes` is the symlink target as raw bytes (analogous to
  `basename`).
- `b"\0"` literals are single NUL bytes used as separators so the input
  cannot be ambiguously parsed.

### Directory child ordering

A directory's child entries are sorted **lexicographically by their raw
basename bytes** before concatenation. The sort uses byte-by-byte
comparison; no locale, no Unicode collation.

### Empty directory exclusion

A directory is **omitted** from its parent's child list if it has no
children that contributed a hash (recursively). This matches the POSIX
tar convention and means scaffolding directories that exist only to
contain other empty scaffolding do not change the tree hash.

A directory with at least one file or symlink underneath it (no matter
how deeply nested) is always included.

### Hidden entries

Hidden entries (`.git`, `.cache`, …) are **not** filtered. A caller that
wants to exclude paths must do so before invoking the hash; the algorithm
itself sees every entry it can `readdir`.

### Unsupported file types

Device files, sockets, FIFOs, and any other non-regular non-symlink
non-directory entries cause an error. The blob hash is intentionally
undefined for trees that contain them.

### Top-level digest

Once the root directory's children have been hashed and concatenated, we
fold them under a versioned prefix:

```text
root_concat = concat(sorted_root_child_hashes)
blob_hash   = "sha256:" || hex(sha256(b"ato-blob-v1\0" || root_concat))
```

The basename of the root directory is **not** part of the input. Two
identical trees stored at different paths produce identical blob hashes.

The on-wire representation is `sha256:<lowercase-hex>`. A future
algorithm change must use a new prefix (e.g. `ato-blob-v2`); both the
prefix string and the on-wire algorithm tag must change together.

### What is deliberately ignored

- mtime, atime, ctime
- owner uid/gid
- permission bits other than `S_IXUSR`
- extended attributes / ACLs
- file system layout (hardlinks vs separate inodes)

If any of these need to influence integrity, they belong in
`derivation_hash`, not `blob_hash`.

### Algorithm agility

`sha256` is the only algorithm accepted for v1. Future algorithms must
ship as a new top-level prefix (e.g. `ato-blob-v2`) and a new on-wire
tag. Mixing algorithms within one tree is not allowed.

## Consequences

- The hash is cheap to compute (one SHA-256 sweep, in directory order)
  and cheap to verify (re-walk the tree, recompute, compare).
- A tree with thousands of empty scaffolding directories hashes the same
  as the same tree with those directories absent. This matches the
  semantics of source-control systems like git that do not represent
  empty directories.
- A symlink that points outside the tree is still hashed — only the
  target path bytes participate in the hash; the algorithm never follows
  symlinks. This means a tree with stale absolute symlinks still hashes
  deterministically; whether to *trust* such a tree is a separate policy
  decision.
- File contents are read into memory once each. Trees with extremely
  large individual files may benefit from a streaming SHA-256 in a
  future revision; that revision MUST produce the same `blob_hash` as
  the in-memory implementation specified here.

## Reference implementation

`crates/capsule-core/src/foundation/blob/tree_hash.rs` is the canonical
reference implementation. It is gated by tests in
`crates/capsule-core/tests/blob_freeze.rs` covering:

- two consecutive freezes of the same input yield the same hash,
- a one-byte content change changes the hash,
- mtime / atime / ctime drift does not change the hash,
- the executable bit changes the hash,
- symlink targets change the hash; symlink chasing does not occur,
- zero-byte files hash deterministically,
- recursively empty directories are excluded.

Any divergence between this document and the reference implementation is
a bug in the implementation; the document wins.
