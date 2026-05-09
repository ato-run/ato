# Execution Identity

## Overview

Execution Identity is the launch-envelope identity Ato uses to answer “are these
launch conditions the same?” It is not just a source hash; it also includes the
runtime, environment, filesystem, policy, and launch shape. The current default
receipt path emits schema v2 unless `ATO_RECEIPT_SCHEMA=v1` forces the older
view.

## How it works

Execution identity is computed before launch. It covers the full launch
condition, not only the source tree:

- source tree
- dependency derivation
- runtime identity
- environment closure
- filesystem view
- network policy
- capability policy
- entry point / argv / working directory

The receipt builder now composes this in one place:

1. derive a launch spec
2. observe source, dependencies, runtime, environment, filesystem, and policy
3. derive launch identity and reproducibility
4. canonicalize the projection with JCS
5. hash it with `blake3-256`

On the desktop side, receipt-aware launches keep `execution_id` in the surface
metadata and map it to `~/.ato/executions/<execution_id>/receipt.json`.

## Specification

- `execution_id` MUST identify launch conditions, not just source content.
- execution receipts MUST be addressable by `execution_id`.
- the current identity header uses JCS canonicalization and `blake3-256`.
- receipt schema selection defaults to v2 and supports v1 only as an opt-out.
- runtime identity v2 MUST carry resolved runtime information and completeness
  status, not only a declared version string.
- secret values MUST NOT be recorded directly in receipts.

References:

- [`rfcs/draft/beyond-reproducible-build.ja.md`](rfcs/draft/beyond-reproducible-build.ja.md)
- [`crates/ato-desktop/src/orchestrator.rs`](https://github.com/ato-run/ato/blob/main/crates/ato-desktop/src/orchestrator.rs)

## Design Notes

“Same code” is not enough to describe reproducibility. What Ato wants to retain
is not only artifact equality, but the shape of the world a launch was about to
observe. The v2 receipt path pushes further in that direction by adding source
provenance, local locator data, richer env / filesystem structure, and runtime
completeness metadata.

## Graph-based execution identity (v0.6.0)

The v0.6.0 core migration (ato-run/ato#74) introduces a typed `ExecutionGraph`
in `capsule-core` (#97) and a canonical form over it (#98). The canonical form
is the authoritative input to graph-derived execution IDs. This section is the
single source of truth for canonicalization; the executable counterpart is
`crates/capsule-core/src/engine/execution_graph/canonical.rs`. **If you change
one, change the other in the same commit.**

### Three identity domains

The same logical execution has three identity views, each computed by hashing
the graph's canonical form under a different domain tag:

- `declared_execution_id = H(canonicalize(G_declared, Domain::Declared))` —
  derived from the manifest, the lock, and policy only. It is host-independent
  and stable across machines that resolve to different concrete artifacts. Two
  developers with the same manifest + lock + policy MUST observe the same
  declared id.
- `resolved_execution_id = H(canonicalize(G_resolved, Domain::Resolved))` —
  derived after host resolution: artifact selectors are replaced with their
  concrete materializations (resolved tool capsule content hashes, runtime
  store paths, platform tuples, etc.). It is host-bound by construction.
- `observed_execution_id = H(canonicalize(G_observed, Domain::Observed))` —
  derived after runtime observation of undeclared edges (e.g. an env var the
  process actually read, an unannounced filesystem touch). Optional. Absent
  by default in v0.6.0; the observed view is `None` on receipts unless
  observation hooks are explicitly enabled. v0.6.0 does not implement those
  hooks (Phase 4 in the umbrella tracker).

Domain separation is enforced inside canonicalization: each canonical form
embeds its domain discriminant before any node/edge bytes, so the same graph
in two different domains yields two different digests. A `Declared` digest
cannot be confused with a `Resolved` digest at the byte level.

### Canonicalization rules

Given an `ExecutionGraph` and a domain, the canonical bytes are produced by:

1. **Header**: 16-byte magic (`ato-graph-canon\0`) || `CANONICAL_FORM_VERSION`
   as little-endian u32 || domain discriminant as u8.
2. **Nodes section**: tag `NODE` (4 bytes) || count as little-endian u32 ||
   for each node: kind discriminant (u8) || node payload. Nodes are sorted by
   `(kind_discriminant, identifier)`. Today every node carries only an
   `identifier`; the payload is therefore a single length-prefixed UTF-8
   string. Future per-variant fields MUST be appended after the identifier.
3. **Edges section**: tag `EDGE` || count u32 || for each edge: source
   (length-prefixed string) || target (length-prefixed string) || edge kind
   discriminant (u8). Edges are sorted by
   `(source, target, edge_kind_discriminant)`.
4. **Labels section**: tag `LBLS` || count u32 || for each `(key, value)` in
   key order: length-prefixed key || length-prefixed value. The graph stores
   labels in a `BTreeMap<String, String>`, so iteration is already sorted by
   key; the spec pins that contract.
5. **Constraints section**: tag `CSTR` || count u32 || for each constraint:
   length-prefixed kind || length-prefixed target. Constraints are sorted by
   `(kind, target)`. The constraint vocabulary is still expanding (#98); when
   it does, additional fields MUST be appended after `target` to keep the
   section additive.

Length-prefixed strings use a 4-byte little-endian u32 length followed by the
raw UTF-8 bytes. Length prefixing is what makes the framing unambiguous under
concatenation: two adjacent strings cannot be confused with a single longer
string because the boundary is encoded explicitly.

The digest is `SHA-256` over the full canonical bytes.

### Secret redaction boundary

Some node kinds may carry secret-bearing fields at runtime; canonicalization
operates on a *redacted* projection. The current redaction list is:

- `Env { identifier }` — only the env var *identifier* (name) participates
  in canonicalization. If the type is later extended with a `value` field,
  that value MUST be omitted from the canonical bytes.
- Any future field carrying a secret (capability tokens, credential blobs,
  etc.) MUST be redacted before canonicalization. Adding such a field
  without updating this list is a spec violation.

The redaction boundary lives in the canonicalizer, not in the type. This is
deliberate: the in-memory graph still needs the raw values at runtime (e.g.
to inject env into a child process); canonicalization is the choke point
that ensures secrets never reach the digest input.

### Declared graph keeps platform selectors

`G_declared` records the platform *selector* it was authored against
(e.g. `linux-x86_64-glibc-*`). `G_resolved` replaces that selector with the
concrete materialization (e.g. the resolved tool capsule's content hash + a
specific platform tuple).

Two consequences fall out of this:

- Reordering an artifact in a manifest does not change `declared_execution_id`,
  because canonicalization sorts nodes and edges deterministically.
- Selecting a different concrete artifact for the same selector *does* change
  `resolved_execution_id`, because the resolved node identifiers carry the
  concrete materialization.

### Observed graph is optional and absent by default

In v0.6.0 there is no observed-runtime computation: `observed_execution_id`
is `None` on every receipt unless a future observation feature is explicitly
enabled. The canonical form for the `Observed` domain is well-defined (it
exists in `CanonicalGraphDomain`), but no production code path emits an
observed graph yet.

This issue (#98) deliberately stops at the canonical form. Receipt plumbing
(`declared_execution_id` / `resolved_execution_id` fields on
`ExecutionReceiptV2`) is Wave 3 (PR-5a). Observation hooks are Phase 4 in
the umbrella tracker.

### Forward compatibility

- New node kinds and new edge kinds are additive. Adding a kind shifts no
  existing discriminant, so digests for graphs that do not use the new kind
  remain unchanged.
- A new per-variant field on an existing node kind is additive only if it is
  appended after the existing payload bytes and only emitted when present.
  Anything else (changing the order of an existing payload, removing a
  field) requires a `CANONICAL_FORM_VERSION` bump.
- `CANONICAL_FORM_VERSION` is part of the framing, so any bump invalidates
  every previously computed digest. That is the intended safety net: a
  format change must produce a different identity, never a silent collision.
- The classic JCS + blake3 receipt-identity path described above is
  unaffected by this section. It coexists with the graph-derived ids until
  the migration in Wave 3 is complete.
