# Hash and Source Provenance Policy

**Status:** Draft decision note  
**Date:** 2026-05-02  
**Scope:** Git commit SHA の扱い、Ato internal CAS hash、source provenance、normalized tree identity、Git LFS / package ecosystem hash との境界  
**Related:** [CAPSULE_HANDLE_SPEC.md](../accepted/CAPSULE_HANDLE_SPEC.md), [CAPSULE_FORMAT_V2.md](../accepted/CAPSULE_FORMAT_V2.md), [DEPENDENCY_DERIVATION_CACHE.md](DEPENDENCY_DERIVATION_CACHE.md), [ATO_HOME_LAYOUT.md](ATO_HOME_LAYOUT.md)

## 1. 結論

Ato は Git commit hash を source provenance / source locator として尊重する。ただし Ato の internal CAS identity、payload integrity、trust model は Git hash に依存しない。

Policy:

1. `github.com` authority の capsule URL は Git commit SHA を受け入れる。
2. Git commit SHA は Ato content integrity hash ではない。
3. `store/blobs/` identity は Ato-managed hash (`sha256:` など) を使う。
4. Directory identity は Git tree 形式そのものではなく、Ato normalized tree hash として定義する。
5. Git から借りるのは object/ref/tree/GC の設計パターンであり、Git object hash そのものではない。

Short version:

```text
Git commit SHA     = source locator / provenance
Ato source tree    = sha256(AtoNormalizedTree)
Ato payload_hash   = sha256(payload artifact bytes)
Ato blob_hash      = sha256(store blob payload)
```

## 2. Hash Domains

Ato では hash の用途を domain ごとに分ける。

| Domain | Example | Owner | Purpose |
| --- | --- | --- | --- |
| Source authority identity | Git commit SHA in `capsule://github.com/...@<sha>` | Git / GitHub | Point-in-time source locator |
| Source tree identity | `sha256:<AtoNormalizedTree>` | Ato | Materialized source content identity |
| Capsule payload integrity | `payload_hash = "sha256:<HEX>"` | Ato | `.capsule` payload verification |
| Store blob identity | `store/blobs/<blob-hash>/` | Ato | Internal immutable payload identity |
| Dependency derivation identity | `derivation_hash = sha256(JCS(inputs))` | Ato | Install input identity |
| Package ecosystem integrity | npm `integrity`, PyPI wheel hash | package manager / registry | External package verification |

Do not collapse these domains into one hash.

## 3. Git Hash Usage Policy

### 3.1 Allowed: Git commit SHA as source locator

`github.com` authority accepts only immutable commit SHA.

```text
capsule://github.com/acme/app@a1b2c3d4e5f6789012345678901234567890abcd
```

This is a source reference, not the final integrity proof for the runnable capsule.

Correct interpretation:

```text
source_ref = github.com/acme/app@<commit-sha>
```

Not:

```text
payload_hash = <commit-sha>
```

### 3.2 Disallowed: Git branch or tag as identity

Branches and tags are mutable references. They are not valid point-in-time identity.

Invalid:

```text
capsule://github.com/acme/app@main
capsule://github.com/acme/app@v1.2.3
```

If a tag is used during authoring, resolver must resolve it to the commit SHA and write the commit SHA into lock/resolved metadata.

### 3.3 Disallowed: Git blob/tree hash as `store/blobs` identity

Git object hashes are not used for Ato internal store identity.

Reasons:

1. Git SHA-1 is historically broken for security purposes.
2. Git SHA-256 repositories exist, but ecosystem support is mixed.
3. Git blob hash includes Git object header (`"blob <size>\0"`) and is not raw content hash.
4. Git tree hash follows Git-specific object semantics.
5. Git LFS stores pointer files in Git object DB, not necessarily the large file content Ato will execute.
6. `.gitattributes` filters, eol normalization, and LFS smudge/clean can make Git object bytes differ from materialized working tree bytes.

Therefore:

```text
store/blobs/<git-object-sha>/        # forbidden
store/blobs/<ato-blob-hash>/         # required
```

## 4. Git Commit SHA Is Not Tree Content Identity

Git commit SHA identifies the full commit object:

```text
commit = tree + parent(s) + author + committer + timestamp + message
```

Two commits can have identical file trees and different commit SHA values because parent, timestamp, author, committer, or message changed.

Therefore Git commit SHA should be described as:

```text
Git repository commit object identity
```

Not:

```text
directory content hash
```

Ato may store both:

```json
{
  "source_ref": "github.com/acme/app@a1b2c3d4e5f6789012345678901234567890abcd",
  "source_tree_hash": "sha256:...",
  "payload_hash": "sha256:..."
}
```

## 5. Ato Normalized Tree Hash

Ato can borrow Git's tree-hash idea without adopting Git's tree object format.

### 5.1 Entry model

```text
AtoTreeEntry {
  kind: file | dir | symlink,
  path: normalized UTF-8 relative path,
  mode: executable-bit-only,
  digest: sha256(file bytes) | child tree hash | sha256(symlink target bytes)
}
```

Canonicalization:

1. Paths are relative to the materialization root.
2. Path separators are `/`.
3. Entries are sorted by normalized path bytes.
4. File digest is over raw materialized file bytes.
5. Symlink digest is over the symlink target string, not target file bytes.
6. Executable bit may be included; other mode bits are ignored unless a future spec says otherwise.

Excluded by default:

- mtime
- uid / gid
- platform xattrs
- macOS quarantine attributes
- ACLs
- editor temporary files
- OS metadata files (`.DS_Store` etc.)

### 5.2 Purpose

`source_tree_hash` is useful for:

- verifying materialized source after checkout
- detecting source tree drift after Git LFS resolution
- stable build/materialization inputs
- audit logs

It is not the same as Git tree SHA.

## 6. Capsule Payload Integrity

Capsule artifact integrity follows [CAPSULE_FORMAT_V2.md](../accepted/CAPSULE_FORMAT_V2.md).

```json
{
  "manifest_hash": "sha256:<HEX>",
  "payload_hash": "sha256:<HEX>"
}
```

`payload_hash` is computed over the capsule payload artifact as defined by the capsule format. It is independent from Git commit SHA and source tree hash.

Recommended metadata relationship:

```text
source_ref       -> where source came from
source_tree_hash -> what source materialized to
payload_hash     -> what artifact was signed and unpacked
```

Trust decisions should use Ato hash/signature/policy, not Git commit SHA alone.

## 7. Git LFS Policy

Git LFS stores pointer files in Git, while large file content lives in LFS storage.

Pointer example:

```text
version https://git-lfs.github.com/spec/v1
oid sha256:...
size ...
```

For executable capsule source, Ato must choose one of two modes.

| Mode | Meaning | Use case |
| --- | --- | --- |
| `lfs-resolved` | Fetch LFS objects and include materialized files in source tree hash / payload | normal executable capsule |
| `lfs-pointer` | Keep pointer files as source | tooling that intentionally works with LFS pointers |

Default should be `lfs-resolved` unless manifest or policy explicitly says otherwise.

Important consequence:

```text
Git tree hash != Ato source_tree_hash after LFS resolution
```

This is expected.

Large assets that should not be embedded in a capsule payload should be declared as Ato `resource` objects, not silently hidden behind Git LFS pointers.

## 8. Patterns Borrowed From Git

Ato should borrow these Git design patterns.

### 8.1 Immutable object / mutable ref split

Git:

```text
.git/objects/<hash>       immutable object
.git/refs/heads/main      mutable ref
```

Ato:

```text
store/blobs/<blob-hash>/  immutable payload
store/refs/...            mutable mapping
store/meta/...            observation metadata
```

This pattern is already used by [DEPENDENCY_DERIVATION_CACHE.md](DEPENDENCY_DERIVATION_CACHE.md).

### 8.2 Mark-and-sweep GC

Git GC traverses reachable objects from refs. Ato GC should do the same.

```text
roots = store/refs/installed + store/refs/pins + active runs
mark reachable blobs
sweep unreachable blobs
clean dangling refs/meta
```

### 8.3 Loose vs packed storage

Git keeps active loose objects and later compacts into pack files.

Ato may later introduce cold blob packing:

```text
store/blobs/<blob-hash>/       hot loose payload
store/packs/<pack-id>.pack     cold packed payloads
store/refs/packs/...           pack index
```

This is a future storage optimization, not a v0 requirement.

### 8.4 Delta encoding

Git pack files delta-compress related objects. Ato may later use deltas between versions of the same capsule/resource.

This is also a future optimization and must not leak into the public trust model.

## 9. Patterns Not Adopted From Git

Do not adopt:

1. Git SHA-1 as Ato security hash.
2. Git object hash format for Ato blobs.
3. Git wire protocol for Ato store fetch.
4. Git branch/merge model for capsule identity.
5. Git index / working tree model as a user-facing Ato model.
6. Git tree SHA as Ato source tree hash.

## 10. Package Ecosystem Hashes

Ato does not force all external ecosystems into Ato hash identity.

| Ecosystem | Native hash/integrity | Ato policy |
| --- | --- | --- |
| Git | commit SHA | source locator/provenance only |
| npm | `integrity` field, usually SHA-512 | package manager verifies; Ato records derivation/output |
| PyPI | wheel hashes / lock data | uv or lockfile verifies; Ato records derivation/output |
| Ato capsule | `sha256` payload / manifest hash | Ato verifies and signs |

Ato's domain is capsule materialization. External package managers remain responsible for ecosystem-specific package verification.

## 11. Source Metadata Example

Possible materialization metadata:

```json
{
  "source": {
    "authority": "github.com",
    "repository": "acme/app",
    "commit": "a1b2c3d4e5f6789012345678901234567890abcd",
    "lfs_mode": "lfs-resolved"
  },
  "source_tree_hash": "sha256:...",
  "payload_hash": "sha256:...",
  "capsule_blob": "sha256:..."
}
```

The commit is provenance. The Ato hashes are integrity.

## 12. Open Decisions

1. Hash algorithm label for Ato tree/blob identity: `sha256` only, or `sha256` now with `blake3` later.
2. Exact binary encoding for `AtoNormalizedTree` before hashing.
3. Whether `source_tree_hash` should be mandatory for Git-backed capsules.
4. How to expose Git LFS resolution mode in `capsule.toml` or lock metadata.
5. Whether Ato should record Git's own tree SHA as auxiliary debug metadata.

## 13. Summary

Git hash is useful in Ato only at the source authority boundary. Ato should accept Git commit SHA for `github.com` source identity and record it as provenance, but should compute its own source tree hash, payload hash, blob hash, and derivation hash.

The right borrowing from Git is architectural, not cryptographic: immutable objects, mutable refs, tree hashing, mark-and-sweep GC, and optional future packing. Ato should keep its internal CAS and trust model under Ato-managed hash algorithms and signatures.