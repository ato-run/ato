# Tier 03 — Limitations

Runnable samples that **fail on purpose**. Each one surfaces a specific, honest limit of `ato` so users discover it here instead of in production.

This tier is a deliberate differentiator: no surveyed competitor ships this. It aligns with `ato`'s fail-closed/professional-honesty stance.

## Samples (planned)

| Slug | Runtime | What it proves we cannot do |
|---|---|---|
| `ipv6-blocked` | `source/node` | IPv6 targets are fail-closed by design |
| `domain-not-in-allowlist` | `source/node` | DNS-denied outside declared `allow_domains` |
| `missing-env-preflight-failure` | `source/node` | Execution stopped pre-launch when required env is absent |
| `cdn-backed-api-changing-ips` | `source/node` | IP rotation is not tracked (startup-only resolution) |
| `native-ffi-not-supported-in-sandbox` | `source/python` | Privileged ops denied in Tier 2 sandbox |

Each sample's `health.toml` sets `expect.exit_nonzero = true` and asserts the specific error pattern via `stderr_regex`.
