# ADR-023 — Source resolver v2: contained symlinks

**Status**: proposed
**Context**: P0 Formation benchmark (`docs/ops/formation-p0-benchmark-2026-09-23.md`):
3 of 20 upstream sources were refused at freeze for containing a symlink
(searxng, navidrome, code-server), 4 more contain one. Code:
`lib/formation/src/source.rs`, `apps/formation-worker/src/job.rs` (`copy_tree`).

## The question

Resolver v1 (`ato.source-resolver.v1`) defines a source tree as regular files
and directories, and refuses everything else — including a symlink that
points at a sibling file. Real repositories carry such links (searxng:
`utils/templates/etc/apache2 -> httpd`). Accepting them is not a parser fix:
it changes what a source tree *is*, and therefore what its identity covers.

## The decision

### What v2 admits

`ato.source-resolver.v2` = v1 + **contained relative symlinks** as entries of
their own. A link at tree path `p` with target `t` is contained when:

- `t` is non-empty, UTF-8, has no NUL, is not absolute and has no `\`;
- `t` reads as zero or more `..` components followed only by plain names
  (`.` is ignored): no `..` after a name (`a/../b` is refused even when it
  would land inside);
- the number of leading `..` is at most the number of parent directories of
  `p` (after the transport wrapper, e.g. `<repo>-<sha>/`, is stripped);
- and no entry of the tree lies beneath a symlink (`dir -> real` with
  `dir/file` is refused: extraction would write through the link).

Why the shape rule and not a per-link lexical resolution: links compose on a
disk. `s -> .` and `a -> s/s/../..` both resolve inside the tree lexically,
but `a` climbs out through `s` when the OS follows it. With the shape rule, a
leading `..` walks up *real* directories only (no entry lies beneath a link,
so a link's parents are real), and plain names never walk up; by induction,
any chain of admitted links stays inside the tree.

Still refused: absolute targets, climbing targets, hard links, devices,
FIFOs (`source_symlink_escape`, `source_unsupported_entry`).

### Identity

A link is measured as `(path, "symlink:" + target)` — the kind and the target
string, never the content it points at. `link -> a` and `link -> b` are
different trees even when `a` and `b` hold the same bytes, and a link is not
the same tree as a copy of its target. `symlink:` cannot collide with a
file's `sha256:` value; fields stay length-prefixed. Links count against
`max_files`.

### Which version a tree is measured under

**The lowest contract whose entry vocabulary covers the tree.** A tree with no
symlink is measured under v1, exactly as before; a tree with at least one,
under v2. The contract string is part of both the tree digest and the
closure ref, and `TreeVerifiedArchive` now carries the contract it measured
under instead of assuming v1.

Why this is canonical rather than an implicit switch:

- it is a pure function of the tree's entries — the same files and links give
  the same version and digest whether the tree arrives as a local directory
  snapshot or an uploaded / codeload archive (tested);
- every tree v1 could measure keeps its v1 digest bit for bit (a test
  recomputes the v1 digest independently), so no recorded v1 closure, source
  revision or derivation ref changes meaning and nothing needs migrating;
- a tree v1 refused had no identity before; it gets a v2 one now.

The alternative — every new acquisition under v2 — would re-identify every
existing source (and every derivation and contract ref built on it) for no
semantic gain on symlink-free trees.

### Materialization

- `expand_archive` recreates an admitted link as a link (re-checking
  containment itself), never as a copy.
- `copy_tree` (source → staged build workspace, workspace → realization)
  recreates links instead of skipping them.
- The build sandbox binds the staged workspace and the read-only source only;
  a link placed there past the resolver still cannot reach the host (tested:
  absolute and climbing links to a host canary are unreadable from a build).

## Consequences and limits

- **Artifact format**: `pack_tree` (the deterministic artifact of a verified
  process workspace) still has no symlink entry and refuses one. A candidate
  whose source carries a link can be built, realized and verified, but its
  artifact cannot yet be stored (`artifact_store_failed`). Extending the
  artifact format is a separate decision, because Runners consume it.
- **Control plane**: ato-api's submission wizard records
  `resolver_contract_version` as a fixed literal
  (`ato.capsule-program-source-projection/v1`), not the version this crate
  measured under. Hosted sources with symlinks are therefore not accepted end
  to end until the control plane carries the measured version; this change
  does not alter the hosted path for symlink-free trees.
- Windows cannot materialize links (refused with a message).
