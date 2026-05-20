# RFC: Split dependency_derivation_hash vs output_hash

**Status**: Draft  
**Issue**: ato-run/ato#33  
**Related**: #74 (v0.6.0 umbrella), `ExecutionReceiptV2`

---

## Problem

The current `ExecutionReceiptV2` and `ExecutionGraph` use a single content hash
that conflates two logically distinct concepts:

1. **`dependency_derivation_hash`** — a hash of all *inputs* that determine
   which version of a dependency is resolved: manifest declarations, lock file
   contents, pinned versions, host platform, policy. This hash should be stable
   across re-runs that produce identical dependency graphs, even if the output
   binaries differ (e.g. non-deterministic builds).

2. **`output_hash`** — a hash of the actual *materialized artifacts*: installed
   packages, compiled binaries, generated files. This hash changes whenever the
   build toolchain or upstream packages emit different bytes.

Mixing the two causes false cache misses (when only the output changed) and
false cache hits (when the derivation inputs changed but the hash was not
recomputed).

---

## Proposed Design

### Layer model

```
capsule.toml + ato.lock.json + host_fingerprint + policy
           │
           ▼
  dependency_derivation_hash   ← deterministic from declared inputs
           │
           ▼
     (build / install)
           │
           ▼
      output_hash              ← content-addressable artifact hash
```

Both hashes are stored in `ExecutionReceiptV2` and `StoredExecutionGraph`.

### Schema changes

```rust
pub struct ExecutionReceiptV2 {
    // ... existing fields ...

    /// Hash of all inputs that determine dependency resolution.
    /// Stable across re-runs with identical inputs; changes when
    /// manifest, lock, platform, or policy change.
    pub dependency_derivation_hash: String,

    /// Hash of materialized artifacts after build/install.
    /// May differ from `dependency_derivation_hash` for non-deterministic
    /// build tools.
    pub output_hash: Option<String>,
}
```

`output_hash` is `Option` because it is not available at session *start* time —
only after the install/build phase completes.

### Computation

**`dependency_derivation_hash`** is computed by `ExecutionGraphBuilder` from:
- `FilesystemIdentityBuilder` output (source tree hash, lock content hash)
- `PolicyIdentityBuilder` output (sandbox mode, policy flags)
- Host fingerprint (OS, arch, libc variant)
- Declared tool versions from `ato.lock.json`

**`output_hash`** is computed at the end of the materialization phase by
hashing the dependency projection directory (`sha256_dir` over the materialized
package store).

### Cache implications

A cache layer can:
- Use `dependency_derivation_hash` as the *lookup key* (fast pre-fetch check).
- Use `output_hash` as the *verification key* (integrity check after restore).

If `output_hash` matches a cached entry, the materialization phase can be
skipped entirely.

---

## Migration

1. Add both fields to `ExecutionReceiptV2` and `StoredExecutionGraph`.
2. `dependency_derivation_hash` is populated in Phase 1 (`ExecutionGraphBuilder`).
3. `output_hash` is populated at end of Phase 3 (materialization complete).
4. Old receipts without `output_hash` are treated as cache misses.

---

## Open Questions

- Should `output_hash` cover *all* materialized files, or only the dependency
  projection? (Recommend: dependency projection only, to keep it reproducible.)
- Should we support a `verify_output_hash` flag in `ato run` to opt into
  integrity checking on cache restore?
