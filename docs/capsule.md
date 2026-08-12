# Capsule

## Overview

A capsule is the runnable unit produced by resolving a recipe over source inputs.

Historically, Ato documentation used "capsule" as the main authoring and sharing
unit. In the current model, the authoring, sharing, and review unit is the
**recipe**. `capsule.toml` remains the local recipe file format for compatibility.

See [Recipes](recipe.md) for the current authoring and Store model.

## How it works

A capsule is what Ato materializes when it resolves a recipe against source
inputs and a user environment. In that sense, a capsule is the runtime
instantiation of a recipe — the concrete, runnable thing that results from
applying a recipe to inputs.

- `capsule.toml` remains the local file format for defining a recipe
- routing still includes compatibility bridges and lock-derived manifests
- `route_manifest*()` loads a manifest, resolves the effective target, and
  synthesizes a runtime model for routing

## Specification

- a capsule is materialized from a recipe and source inputs
- `capsule.toml` is a recipe file; it is not necessarily one-to-one with a repository
- the current manifest model centers on `schema_version = "0.3"`
- a manifest MUST resolve a non-empty `default_target` that exists under `[targets]`
- runtime-specific fields MUST route into one of the current runtime kinds:
  `source`, `wasm`, `oci`, or `web`
- current capsule types include `app`, `tool`, `inference`, `job`, and `library`

References:

- [`rfcs/accepted/CAPSULE_SPEC.md`](rfcs/accepted/CAPSULE_SPEC.md)
- [legacy Capsule Artifact Format v2](rfcs/archived/CAPSULE_FORMAT_V2.md)
- [`rfcs/accepted/CAPSULE_DEPENDENCY_CONTRACTS.md`](rfcs/accepted/CAPSULE_DEPENDENCY_CONTRACTS.md)
- [Recipes](recipe.md)

## Design Notes

The capsule model gives Ato a single shape for apps, tools, and services.
Declaration, resolution, execution, and sharing all stay inside the same shape.
The compatibility bridge in the router exists to preserve that single shape even
when the raw input comes from a flat v0.3 draft surface or a lock-derived
execution descriptor.

In the 0.6.0 model, "capsule" refers to the materialized runtime unit, while
"recipe" refers to the authored, shareable declaration. The file name
`capsule.toml` is preserved for compatibility; the mental model treats it as a
recipe file.
