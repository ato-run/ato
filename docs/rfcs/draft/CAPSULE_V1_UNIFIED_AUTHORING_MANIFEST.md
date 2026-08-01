---
title: "Capsule v1 Unified Authoring Manifest"
status: draft
date: "2026-07-30"
author: "@egamikohsuke"
---

# Capsule v1 Unified Authoring Manifest

## Scope

This RFC extends only `schema_version = "1"` authoring. It does not change
the v0.3 declaration identity defined by ADR-014.

The v1 manifest is the source of truth for execution, source selection, Store
metadata, and Store assets. API listing JSON remains a derived compatibility
projection.

## Manifest additions

```toml
[source]
root = "."
ignore = ["node_modules/**", "dist/**"]

[metadata]
short_description = "A short Store description"
description = "A longer description"
license = "MIT"
tags = ["developer-tool"]

[metadata.store]
category = "developer_tools"
subcategory = "utilities"

[metadata.assets.icon]
path = "assets/icon.png"

[metadata.assets.banner]
url = "https://assets.example/banner.webp"
```

`source` defaults to root `"."` and no manifest ignore rules for backward
compatibility with existing v1 manifests. `metadata` defaults to an empty
value. Asset locators contain exactly one of `path` or `url`.

## Source-selection policy

Manifest patterns use a restricted gitignore grammar: `/`, `**`, negation,
and last-match-wins are supported. Backslashes, parent traversal, empty
patterns, and unbounded pattern sets are rejected.

The platform always applies `ato-source-filter/v1` safety exclusions for Git
metadata, dotenv files, private keys, SSH material, and AWS material. A
manifest negation cannot re-include a system-excluded path.

The source projection is computed after reading and normalizing the root
manifest:

```text
pinned source
  -> read root capsule.toml
  -> normalize source.root and ignore rules
  -> select source bytes
  -> compute source closure
  -> bind source closure and normalized manifest digest
```

The root `capsule.toml` and selected lock remain control files. Their bytes do
not enter the source closure. The normalized manifest digest enters the
Capsule revision identity separately.

## Assets

Repository asset paths are normalized source-relative paths. They must remain
inside the selected source root and must not be excluded by the effective
source policy. URL assets require HTTPS. API-side resolution validates
redirects, address ranges, media type, magic bytes, size, and dimensions, then
binds the revision to the fetched content digest.

## Compatibility

Existing v1 manifests without `source` or `metadata` retain their prior source
selection and execution behavior. Unknown fields remain fail-closed.
