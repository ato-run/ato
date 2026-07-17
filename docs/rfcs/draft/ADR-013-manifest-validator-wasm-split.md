---
title: "ADR-013: Split an I/O-free capsule-manifest-core crate for a WASM validator"
status: draft          # draft | accepted | archived
date: 2026-07-13
author: "@egamikohsuke"
related:
  - "GITHUB_CAPSULE_REQUEST_PIPELINE.md"
  - "../accepted/SCHEMA_REGISTRY.md"
---

# ADR-013: Split an I/O-free `capsule-manifest-core` crate for a WASM validator

## Context

The [GitHub Capsule Request Pipeline](GITHUB_CAPSULE_REQUEST_PIPELINE.md) needs
to validate generated `capsule.toml` candidates as the **final gate** before
QA — and it needs to do so on the ato-api edge (a Cloudflare Worker), where the
only portable execution target is WebAssembly. The manifest validator lives in
the `capsule` crate today, which is not WASM-clean because parts of it touch the
filesystem.

Most of what the Worker needs is already pure: **single-manifest parse and
validation do no I/O**. The one exception is **workspace `[packages]`
delegation**, which reads sibling manifests from disk — the inline
`fs::read_to_string(&delegated_manifest_path)` at
`crates/capsule/src/foundation/types/manifest_v03.rs:1546` (and the
`.canonicalize()` just above it). That single feature is what keeps the
validator out of WASM.

There is precedent for exactly this kind of extraction: **N2 pulled
`ConfigField` / `ConfigKind` out into the `protocol` crate**
(`crates/protocol/src/config.rs`) to share a pure type across boundaries.

## Decision

Split an **I/O-free `capsule-manifest-core`** crate out of `capsule`:

1. **Move the pure validation + single-manifest parse** into
   `capsule-manifest-core` (no `std::fs`, no path canonicalization — WASM-clean).
   `capsule` re-exports from the core so existing callers are unaffected
   (same pattern as the N2 `protocol` extraction).
2. **Inject a resolver trait for `[packages]` delegation.** The workspace
   delegation that needs `fs` becomes a trait the *host* implements. The CLI
   implements it with real filesystem reads; the **WASM build leaves it
   unimplemented**.
3. **MVP rejects `[packages]` manifests server-side** as `blocked_incompatible`.
   The GitHub-request pipeline does not need workspace delegation for its MVP
   lanes (static web / docker-import), so the WASM validator can refuse
   `[packages]` cleanly rather than pretend to resolve it.
4. **`wasm32` target in `rust-toolchain.toml`** so both the WASM artifact and
   its tests build in CI.

### Artifact and cross-repo parity

- The WASM validator is shipped as an **`ato` release asset**, **version- and
  SHA-256-pinned** in ato-api and **recorded in receipts** — so a validation
  result is attributable to an exact validator build.
- **The identical manifest corpus is golden-tested in BOTH repos' CI**: the same
  fixtures must produce the same **normalized TOML**, the same **diagnostic
  codes**, and a **matching `manifest_hash`** whether validated by the native
  CLI or the WASM module. CLI↔WASM divergence is a CI failure.
- **TypeScript TOML parsing is display-only** — the TS side may parse a manifest
  to show it, but it is never an authority. The WASM module (compiled from the
  same Rust core) is the only validation authority on the edge.

### Prerequisite

- **`MANIFEST_SCHEMA_VERSION` constant consolidation is a separate prerequisite
  PR (in flight).** The schema-version constants are currently scattered (e.g.
  `BLOB_MANIFEST_SCHEMA_VERSION` in `crates/capsule/src/foundation/blob/manifest.rs`
  alongside inline `schema_version=0.3` literals); consolidating them lands
  before the core split so the extracted crate carries one canonical constant.

## Alternatives Considered

### Option A: Re-implement validation in TypeScript on the Worker

- Pro: no crate split; native to the Worker runtime.
- Con: two divergent validators, guaranteed to drift; the manifest semantics are
  non-trivial (schema 0.3, workspace rules). Rejected — the pipeline requires
  CLI↔edge byte parity.

### Option B: Make the whole `capsule` crate WASM-clean

- Pro: no new crate.
- Con: `capsule` pulls in far more than manifest validation; forcing all of it
  WASM-clean is a large, risky refactor for a small need. Rejected in favor of a
  focused core extraction.

### Option C: Keep `[packages]` in the WASM validator via a virtual FS

- Pro: full feature parity on the edge.
- Con: a virtual FS on the Worker to resolve sibling manifests is complexity the
  MVP does not need; `[packages]` repos are out of the MVP lanes anyway.
  Rejected — reject `[packages]` as `blocked_incompatible` instead.

## Consequences

- **Good**: one Rust source of truth for manifest validation, compiled to both a
  native CLI and a WASM edge module, with CI-enforced parity.
- **Good**: the split follows the established N2 precedent and keeps `capsule`'s
  public API stable via re-exports.
- **Bad / scope**: `[packages]` (workspace-delegation) manifests are not
  server-validatable in the MVP and are blocked. Supporting them later means
  implementing the resolver trait for the edge (Option C territory).
- **Bad / cost**: a pinned WASM artifact must be built, versioned, and kept in
  lockstep with the golden corpus in both repos.

## Follow-up

- Land the `MANIFEST_SCHEMA_VERSION` consolidation PR first.
- Define the resolver trait so the `fs`-touching `[packages]` path
  (`manifest_v03.rs:1546`) moves behind it without changing CLI behavior.
- Wire the WASM artifact's version+SHA-256 pin into ato-api and receipts.
