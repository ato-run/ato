# Capsule

## Overview

A capsule is the execution unit that gives Ato one model for apps, tools, and
services. Instead of multiplying special cases, Ato treats them through the same
declarative shape.

## How it works

A capsule is described around `capsule.toml`.

- Top-level fields declare the name, type, and default target
- `[targets.<label>]` defines the runtime-specific launch contract
- dependencies and isolation policy can be added when needed

The exact manifest and format rules live in the RFCs.

## Specification

- A capsule MUST be declared through `capsule.toml`.
- A manifest MUST declare at least one runnable target.
- `schema_version`, `name`, `version`, `type`, and `default_target` MUST satisfy the current manifest contract.
- runtime-specific fields MUST follow the accepted manifest and format specs.

References:

- [`rfcs/accepted/CAPSULE_SPEC.md`](rfcs/accepted/CAPSULE_SPEC.md)
- [`rfcs/accepted/CAPSULE_FORMAT_V2.md`](rfcs/accepted/CAPSULE_FORMAT_V2.md)
- [`rfcs/accepted/CAPSULE_DEPENDENCY_CONTRACTS.md`](rfcs/accepted/CAPSULE_DEPENDENCY_CONTRACTS.md)

## Design Notes

Keeping the capsule as the unit means growing a shared model instead of adding
feature-specific exceptions. Declaration, resolution, execution, and sharing all
stay inside the same shape.
