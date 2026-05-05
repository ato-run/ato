# Capsule

## Overview

A capsule is the execution unit that gives Ato one model for apps, tools, and
services. In the current implementation, `capsule.toml` schema v0.3 is the main
authoring surface, but routing still includes compatibility bridges and
lock-derived manifests.

## How it works

A capsule is described around `capsule.toml`.

- Top-level fields declare the name, type, default target, metadata,
  requirements, build, isolation, dependencies, contracts, and workspace setup
- `[targets.<label>]` defines the runtime-specific launch contract
- `route_manifest*()` loads a manifest, resolves the effective target, and
  synthesizes a runtime model for routing
- lock-backed runs can rebuild a compatibility manifest bridge from `ato.lock.json`

The router also supports flat v0.3 surfaces without a `[targets]` table by
normalizing them through the v0.3 compatibility path before execution.

## Specification

- a capsule is primarily declared through `capsule.toml`
- the current manifest model centers on `schema_version = "0.3"`
- a manifest MUST resolve a non-empty `default_target` that exists under `[targets]`
- `version` may be empty in the current struct model, but `name`, `type`, and
  target selection still drive routing
- runtime-specific fields MUST route into one of the current runtime kinds:
  `source`, `wasm`, `oci`, or `web`
- current capsule types include `app`, `tool`, `inference`, `job`, and `library`

References:

- [`rfcs/accepted/CAPSULE_SPEC.md`](rfcs/accepted/CAPSULE_SPEC.md)
- [`rfcs/accepted/CAPSULE_FORMAT_V2.md`](rfcs/accepted/CAPSULE_FORMAT_V2.md)
- [`rfcs/accepted/CAPSULE_DEPENDENCY_CONTRACTS.md`](rfcs/accepted/CAPSULE_DEPENDENCY_CONTRACTS.md)

## Design Notes

Keeping the capsule as the unit means growing a shared model instead of adding
feature-specific exceptions. Declaration, resolution, execution, and sharing all
stay inside the same shape. The compatibility bridge in the router exists to
preserve that single shape even when the raw input comes from a flat v0.3 draft
surface or a lock-derived execution descriptor.
