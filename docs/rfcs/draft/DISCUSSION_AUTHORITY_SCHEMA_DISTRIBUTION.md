---
title: "Discussion: Authority Schema Distribution"
status: discussion
date: "2026-04-21"
author: "@koh0920"
related:
  - "docs/rfcs/draft/CAPSULE_URL_SPEC.md"
---

# Discussion: Authority Schema Distribution for `capsule://` URL Spec

> This is a **pre-RFC discussion document** (Issue-equivalent) to gather
> input before finalizing `CAPSULE_URL_SPEC.md §4.2`. Promote to an RFC
> section only after the distribution mechanism is settled.

## Context

`CAPSULE_URL_SPEC.md` (draft) splits `capsule://` semantics into three
layers:

1. **Grammar** (spec-fixed): scheme, authority, path, version-id syntax
2. **Authority policy** (authority-defined): segment count, reserved names,
   `version-id` grammar, case sensitivity
3. **Functional metadata** (future, `.well-known` candidate): manifest URL
   template, signing root, rate limits — **not** required for identity

§3 establishes that **identity resolution MUST NOT depend on runtime-fetched
state** to preserve the point-in-time identity invariant (§3.1). Therefore
authority policy (Layer 2) cannot be distributed via `.well-known`.

The open question is: **how, concretely, should authority policies be
distributed** so that:

- Tooling across the ecosystem can consistently interpret `capsule://`
  URLs for a given authority.
- Identity resolution remains `.well-known`-free and offline-parseable.
- Updates can propagate without breaking pinned identities.

## Candidates

### Option 1 — Bundled static map (tool-side)

Each tool (ato-cli, desky, ato-docs linter, VSCode extension, etc.)
ships a compile-time map `authority → policy schema`. Schemas are pulled
from well-known upstream repos at tool build time.

- ✅ No runtime IO. Fully offline.
- ✅ Identity of a URL cannot change under a user unless they update
  the tool itself (matches `cargo` / `npm` upgrade model).
- ❌ Each tool maintains its own schema map → drift risk.
- ❌ Adding a new authority requires every tool to upgrade.
- ❌ Private/internal authorities have no good distribution path
  without patching the tool.

### Option 2 — Shared schema registry repo (single source, tools vendor it)

A single repository (e.g. `ato/authority-schemas`) holds all known
authority policies. Each tool vendors it at build time — either as a
git submodule, a Cargo dependency, or a fetched tarball pinned by hash.

- ✅ Same offline guarantees as Option 1.
- ✅ Single source of truth — no drift.
- ✅ New authorities are added by PR to the registry repo; tools pick
  up on their next release.
- ⚠️ Private authorities still require a fork or additional local
  config path.
- ❌ Release cadence is coupled to the registry repo's update cycle.

### Option 3 — User-local `~/.ato/authorities/*.toml` with bundled defaults

Tools ship a default set (Option 1 or 2). Users can drop additional
authority policies into `~/.ato/authorities/` (or configure a search
path). Tools merge these at startup.

- ✅ Offline, identity-stable.
- ✅ Supports private / internal authorities out of the box.
- ⚠️ Risk of local config diverging across a user's machines → need
  clear sync story (e.g. `ato auth import <url>` that fetches and
  pins by content hash).
- ⚠️ Users could shadow a public authority's policy; policy conflicts
  need an explicit precedence rule (e.g. built-in > user override,
  or configurable).

### Option 4 — Hybrid: Option 2 (shared) + Option 3 (local override)

Built-in policies from a shared registry (Option 2) plus optional
per-user overrides (Option 3). This is the model `cargo` uses
(`config.toml` + registries) and `npm` uses (`.npmrc` hierarchy).

- ✅ Covers public ecosystem + private authorities.
- ✅ Familiar to users from `cargo` / `npm`.
- ⚠️ Need precedence rules (suggest: built-in < user-global < project-local
  for public authorities that define the same `authority` host).
- ⚠️ Requires a simple CLI to inspect/import: `ato authority list`,
  `ato authority show <host>`, `ato authority import <url-or-path>`.

## Format

Regardless of distribution, the **schema format** needs to be nailed down.
Proposed skeleton (TOML, matches the style of other ato spec artifacts):

```toml
# authority-schemas/ato.run.toml
schema_version = "1"
authority = "ato.run"
display_name = "ato.run"

[path]
min_segments = 2
max_segments = 2

[[path.segment]]
name = "publisher"
description = "Organization or user publishing the capsule"
pattern = "^[a-z0-9][a-z0-9-]{0,38}$"
case_sensitive = false

[[path.segment]]
name = "slug"
description = "Capsule identifier within the publisher namespace"
pattern = "^[a-z0-9][a-z0-9-]{0,63}$"
case_sensitive = false

[version_id]
# Regex for the syntactic form accepted on this authority.
# §3.2 (mutable reference ban) is still enforced globally.
pattern = "^\\d+\\.\\d+\\.\\d+(?:-[0-9A-Za-z.-]+)?(?:\\+[0-9A-Za-z.-]+)?$"
description = "Exact semver (MAJOR.MINOR.PATCH[-prerelease][+build])"

[reserved]
publishers = ["search", "topic", "user", "store", "api", "registry", "help", "docs", "status"]

[resolver]
# Advisory only — not consulted at identity-parse time.
kind = "ato-store-http"
endpoint_hint = "https://api.ato.run"
```

Open decisions:

- **JSON Schema vs TOML?** TOML is more readable; JSON Schema is easier
  to machine-validate. Could define in TOML and generate a JSON Schema
  for validation.
- **Pattern syntax**: ECMA-262 regex (widest interop) vs RE2 (no
  backtracking, safer).
- **`resolver` block**: should it be in authority schema at all, or
  should it live in a separate "functional metadata" doc (Layer 3) to
  keep identity and resolution strictly separated?

## Questions for Discussion

- **Q1**: Which of Options 1–4 do we commit to for v0.1?
  - Recommendation: **Option 4 (hybrid)**. Matches `cargo` / `npm` mental
    model. Start with a single built-in schema for `ato.run`; add
    `github.com` when its resolver is implemented.
- **Q2**: Where does the shared schema registry live?
  - Candidate: `github.com/ato-run/authority-schemas` with versioned
    releases. Vendored into `apps/ato-cli` via a build script that
    pins by git SHA.
- **Q3** *(resolved by `CAPSULE_URL_SPEC.md §6`)*: How do tools enforce
  schema consistency at identity-parse time vs human-facing rendering time?
  - Identity-parse: syntactic only (grammar from §2, plus
    `version_id.pattern` check). Independent of whether segment names
    are known.
  - Human-facing: use `[[path.segment]]` `name` / `description` to
    render `"publisher 'acme' not found"` instead of `"path segment 1
    'acme' not found"`.
  - *Left open for further discussion only if concrete ambiguity arises
    during implementation.*
- **Q4**: Should user-local overrides be allowed to **add** authorities
  only, or also to **modify** built-in authorities?
  - Recommendation: **add only**. Modifying `ato.run` locally would be
    a silent trust/identity violation.
- **Q5**: Does the schema belong in `apps/uarc/` (alongside the UARC
  manifest schema) or in a new top-level directory?
  - Recommendation: new top-level `schemas/authorities/` to keep UARC
    (manifest format) and authority (URL interpretation) concerns
    separate.

## Proposed Next Actions

1. Review this document; settle Q1–Q5.
2. Promote the agreed mechanism into `CAPSULE_URL_SPEC.md §4.2` as
   normative text (replacing the current placeholder).
3. Open a separate implementation ticket for the schema registry repo
   and the vendoring build script.
4. Land the `ato.run` v1 authority schema as the first concrete
   instance, paired with a renamed `CAPSULE_HANDLE_SPEC.md` ("ato.run
   Authority Policy v1").

## Out of Scope

- `.well-known/capsule-configuration`: explicitly deferred to v0.2.
  Any mention in this document is illustrative only.
- Resolver plugin interface (Rust trait, dynamic loading, etc.): separate
  design, depends on this one.
