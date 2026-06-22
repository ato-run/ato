# Sample Matrix

Auto-generated from each sample's `health.toml`. Do not edit manually — run `node tools/regenerate-matrix.mjs` to refresh.

**Legend:** ✅ full/pass · 🔵 smoke · ⚠️ required/advisory-gap · ❌ expected-fail · — none/skip

## Tier 01 — Capabilities

| Sample | Runtime | Difficulty | Desktop | Docker | Net | Linux | macOS | Windows | min_ato_version | Outcome |
|--------|---------|------------|---------|--------|-----|-------|-------|---------|----------------|---------|
| [`env-preflight`](01-capabilities/env-preflight) | `source/node` | beginner | — | — | — | ✅ | ✅ | — | 0.4.69 | ✅ |
| [`host-bridge-clipboard`](01-capabilities/host-bridge-clipboard) | `source/python` | advanced | ⚠️ | — | — | ✅ | ✅ | — | 0.4.69 | ✅ |
| [`network-policy-allowlist`](01-capabilities/network-policy-allowlist) | `source/node` | beginner | — | — | — | ✅ | ✅ | — | 0.4.69 | ✅ |
| [`oci-alpine-hello`](01-capabilities/oci-alpine-hello) | `oci/runc` | beginner | — | ⚠️ | ⚠️ | ✅ | ✅ | — | 0.4.69 | ✅ |
| [`persistent-workspace`](01-capabilities/persistent-workspace) | `source/python` | beginner | — | — | — | ✅ | ✅ | — | 0.4.69 | ✅ |
| [`runtime-version-pinning`](01-capabilities/runtime-version-pinning) | `source/python` | beginner | — | — | — | ✅ | ✅ | — | 0.4.69 | ✅ |
| [`wasm-hello`](01-capabilities/wasm-hello) | `wasm/wasmtime` | intermediate | — | — | — | ✅ | ✅ | — | 0.4.69 | ✅ |

## Tier 03 — Limitations

| Sample | Runtime | Difficulty | Desktop | Docker | Net | Linux | macOS | Windows | min_ato_version | Outcome |
|--------|---------|------------|---------|--------|-----|-------|-------|---------|----------------|---------|
| [`bad-toml-syntax`](03-limitations/bad-toml-syntax) | `source/node` | beginner | — | — | — | ✅ | ✅ | — | 0.4.69 | ✅ |
| [`missing-env-preflight-failure`](03-limitations/missing-env-preflight-failure) | `source/node` | intermediate | — | — | — | ✅ | ✅ | — | 0.4.69 | [⚠️](docs/upstream-issues/001-required-env-advisory.md) |
| [`missing-required-field`](03-limitations/missing-required-field) | `source/node` | beginner | — | — | — | ✅ | ✅ | — | 0.4.69 | ✅ |
| [`no-raw-gpu-handle`](03-limitations/no-raw-gpu-handle) | `source/python` | intermediate | — | — | — | ✅ | ✅ | — | 0.4.69 | ✅ |
| [`stale-lockfile`](03-limitations/stale-lockfile) | `source/node` | beginner | — | — | — | ✅ | ✅ | — | 0.4.69 | ✅ |

## Advisory Gaps

Samples where ato-cli implementation diverges from spec. See [docs/SAMPLE_FINDINGS.md](docs/SAMPLE_FINDINGS.md) for details.

- [`missing-env-preflight-failure`](03-limitations/missing-env-preflight-failure) — [upstream issue](docs/upstream-issues/001-required-env-advisory.md)
