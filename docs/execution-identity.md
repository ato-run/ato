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
