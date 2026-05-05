# Execution Identity

## Overview

Execution Identity is the launch-envelope identity Ato uses to answer “are these
launch conditions the same?” It is not just a source hash; it also includes the
runtime and environment shape.

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

On the desktop side, receipt-aware launches keep `execution_id` in the surface
metadata and map it to `~/.ato/executions/<execution_id>/receipt.json`.

## Specification

- `execution_id` MUST identify launch conditions, not just source content.
- runtime identity MUST include both declared and resolved runtime identity.
- execution receipts MUST be addressable by `execution_id`.
- secret values MUST NOT be recorded directly in receipts.

References:

- [`rfcs/draft/beyond-reproducible-build.ja.md`](rfcs/draft/beyond-reproducible-build.ja.md)
- [`crates/ato-desktop/src/orchestrator.rs`](https://github.com/ato-run/ato/blob/main/crates/ato-desktop/src/orchestrator.rs)

## Design Notes

“Same code” is not enough to describe reproducibility. What Ato wants to retain
is not only artifact equality, but the shape of the world a launch was about to
observe.
