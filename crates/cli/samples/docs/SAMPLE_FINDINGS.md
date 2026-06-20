# Sample Findings — v0.4.69 Audit

Discrepancies discovered while building and running the `sample-capsules` test suite against ato-cli v0.4.69. Each finding links a sample (observable evidence), describes the gap, and proposes a fix.

---

## Confirmed Discrepancies

### 1. `required_env` is advisory-only — auto-provisioner synthesizes dummy values

**Sample**: `03-limitations/missing-env-preflight-failure`  
**Severity**: High — breaks "Safe by default" philosophy  
**Status**: Pending fix (file issue at ato-run/ato-cli)

**Observed (v0.4.69)**:  
When `required_env = ["OPENAI_API_KEY"]` is declared and `OPENAI_API_KEY` is absent, the auto-provisioner prints:
```
Auto-provisioning issue [app]: prepare synthetic env for OPENAI_API_KEY [safe-default]
```
…and execution continues with a 15-char placeholder. Exit 0.

**Expected**:  
Pre-launch check should fail with a dedicated error code (e.g. `E220`) before the auto-provisioner or consent gate runs, with an actionable message:
```
E220: Required environment variable OPENAI_API_KEY is not set.
Declare it in your shell or add it to your .env before running this capsule.
```

**Proposed fix**: Add `[env].enforcement = "strict" | "advisory"` to `capsule.toml`. Default `strict` in non-TTY mode. When `strict`, bypass auto-provisioner for declared `required_env` vars.

**Impact on samples**: `missing-env-preflight-failure/health.toml` currently has `exit_code = 0`. Once fixed, flip to `exit_nonzero = true` + `stderr_regex = "E220|OPENAI_API_KEY"`.

---

### 2. Non-TTY mode loses TUI error details

**Sample**: all `03-limitations/` samples run in CI (non-interactive)  
**Severity**: Medium — degrades CI diagnostics  
**Status**: Needs investigation (may be intentional)

**Observed**:  
When `ato run .` runs non-interactively (pipe, CI runner), some recoverable errors that normally show a rich TUI panel (e.g. consent dialogs) silently fall through to `E105: non-TTY fallback`. The error details (which keys are missing, which capabilities need consent) are not present in stderr.

**Question**: Is `E105` with no extra detail intentional for non-TTY, or should stderr include a machine-readable JSON blob of the blocked action?

**Impact on samples**: `env-preflight` and `network-policy-allowlist` cannot be fully tested at L2-functional in CI without a TTY or a `--no-interactive` flag that emits structured errors.

---

### 3. `source/node` auto-provisions Deno for non-package-manager entrypoints

**Sample**: `01-capabilities/env-preflight`, `greeter-client`, `greeter-service`  
**Severity**: Low — surprising but documented behavior  
**Status**: Documented (not a bug, but a footgun)

**Observed**:  
`runtime = "source/node"` with a plain `.js` entrypoint (no `resolution.json`) is silently executed by the Deno runtime via `NodeCompat` executor. This means:
- `require()` / CommonJS modules fail (must use ESM)
- `process.env` is unavailable (must use `Deno.env.get()`)
- `node:https` / `node:http` are unavailable as native modules

**Root cause**: `node_compat.rs` falls back to `build_runtime_command` which invokes Deno when `resolution.json` is absent, because Deno is always available while Node.js requires separate provisioning.

**Fix applied in ato-cli** (commit `7ee7141`): `derive.rs` now populates `allow_hosts` for both `(Source, Node)` and `(Source, Deno)`, so `port`-declared capsules get `--allow-net` regardless of which executor runs.

**Documentation needed**: `capsule.toml` reference should note that `source/node` without a lock file uses the Deno-compat path and requires ESM + `fetch()`.

---

## Open Questions

1. **Should `capsule.toml` have a top-level `enforcement_profile = "strict" | "advisory"` field** that affects `required_env`, `network`, and capability checks uniformly? This would be more ergonomic than per-field enforcement flags.

2. **What is the intended exit code for ALL capability-gated rejections?** Currently `E301` is used for "sandbox opt-in required" (Python/native). Should all gated-but-not-run samples exit the same code, or is per-gate differentiation intended?

3. **Is `ato run .` in non-TTY mode ever supposed to auto-consent to network policies?** If so, a `--yes` / `--non-interactive` flag is needed. If not, CI samples that require network access cannot reach their script body.

---

## Sample Status Legend Additions Needed

The existing `health.toml` `[expect]` section lacks a way to express:

| New state | Proposed field |
|-----------|---------------|
| "expected failure, but currently passes due to known gap" | `exit_code = 0` + `[flaky] quarantined = true, issue = "https://..."` |
| "passes only after specific env var is set in CI" | `[env.ci_secrets]` (already in schema ✅) |
| "expected to flip behavior after a future ato-cli version" | `[compat] breaks_after_version = "0.5.0"` |

The `breaks_after_version` field is new and not yet in the schema. Propose adding it to track samples that document **current-but-wrong** behavior.

---

## Change Log

| Date | Finding | Action |
|------|---------|--------|
| 2026-04 | `required_env` advisory-only | Documented in `EXPECTED.md`; issue to file at ato-run/ato-cli |
| 2026-04 | `source/node` → Deno fallback missing `allow_hosts` | Fixed in ato-cli `7ee7141`; samples updated to ESM |
| 2026-04 | All 12 original samples had manifest/lockfile issues | Fixed across commits `27b1663`, `383ab9a` |
