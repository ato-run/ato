# RFC: Ship Provider Host Tools as Nested Capsules

**Status**: Draft  
**Issue**: ato-run/ato#71  
**Related**: #74 (v0.6.0 umbrella), tools registry (capsule)

---

## Problem

Provider host tools — pnpm, yarn, bun, uv, and similar language-ecosystem
tools that Ato downloads and manages — are currently fetched and installed
by ad-hoc logic inside `capsule` (`contract/tools.rs`,
`contract/lockfile_support.rs`).

This approach has several drawbacks:

1. **Ad-hoc fetch logic** per tool; no unified lifecycle (install, verify,
   upgrade, uninstall).
2. **No capsule boundary** — tools run with the same permissions as the Ato
   process.
3. **No audit trail** — tool invocations are not recorded in `ExecutionReceiptV2`.
4. **No content addressing** — fetched binaries are not tracked by the capsule
   store or the lock file.
5. **Not composable** — a user cannot pin, fork, or replace a provider tool
   without modifying Ato source code.

---

## Proposed Design

Every provider host tool becomes a **system capsule** — a capsule with a
well-known `capsule_id` under the `system.ato.run` namespace, declared in
`capsule.toml` under `[[tools]]` and resolved in `ato.lock.json`.

### capsule.toml declaration (consumer side)

```toml
[[tools]]
name = "pnpm"
version = "9.9.0"
capsule_id = "system.ato.run/tools/pnpm"
```

This is equivalent to what `RuntimeToolSpec` expresses today, but expressed as
a first-class capsule dependency.

### System capsule manifest (`system.ato.run/tools/pnpm`)

```toml
[capsule]
id = "system.ato.run/tools/pnpm"
version = "9.9.0"

[execution]
runtime = "source/native"
driver = "prebuilt"

[distribution]
platforms = ["darwin-aarch64", "darwin-x64", "linux-x64", "linux-aarch64", "windows-x64"]

[[artifacts]]
platform = "darwin-aarch64"
url = "https://registry.npmjs.org/pnpm/-/pnpm-9.9.0.tgz"
sha256 = "..."
```

### Resolution flow

```
ato run my-capsule
  │
  ├── resolve [[tools]] from ato.lock.json
  │     → "system.ato.run/tools/pnpm@9.9.0"
  │
  ├── check local store: ~/.ato/store/system.ato.run/tools/pnpm/9.9.0/
  │     hit → return ToolHandle
  │     miss → fetch + install nested capsule
  │
  └── invoke tool via capsule handle
```

### Boundary enforcement

Nested capsules run with a **reduced capability set**:

```toml
[policy]
network = "none"         # no network access during invocation
filesystem = "read"      # read the project dir; write only to store dir
```

This means `pnpm fetch` runs with network disabled after the download phase —
only the pre-fetched store dir is accessible.

### Audit trail

Each tool invocation emits a `ToolInvocationRecord` in `ExecutionReceiptV2`:

```rust
pub struct ToolInvocationRecord {
    pub capsule_id: String,
    pub version: String,
    pub binary_sha256: String,
    pub args: Vec<String>,
    pub duration_ms: u64,
    pub exit_code: i32,
}
```

---

## Migration Path

1. **Slice A** (current): `RuntimeToolSpec` in `contract/tools.rs` — ad-hoc
   fetch, no capsule boundary.
2. **Slice B** (this RFC): Define system capsule manifests for pnpm/yarn/bun/uv.
   Resolution still done by `ensure_runtime_tool` but validates against the
   capsule store.
3. **Slice C**: Full nested capsule execution — tools run inside a nacelle
   sandbox.

The public API (`ToolHandle`, `ensure_runtime_tool`) remains unchanged. The
fetch and validation internals are progressively replaced.

---

## Open Questions

- Should system capsule manifests live in `uarc/` (the schema repo) or in
  `ato-api/` (the store backend)?
- Should `ato lock` auto-resolve `[[tools]]` entries, or should tool versions
  be pinned manually?
- How does this interact with the existing `~/.ato/tools/` directory layout
  vs `~/.ato/store/`?
