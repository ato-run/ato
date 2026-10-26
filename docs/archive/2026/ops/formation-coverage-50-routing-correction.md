# 6b-C appendix — npm v1 routing correction (LibreChat)

**Claim: restored npm v1 compatibility lost in #1440.** A regression
correction, not a coverage improvement and not a new capability. The 50-app
ledger ([formation-coverage-50](formation-coverage-50.md)) is unchanged; its
LibreChat result remains the observation under `c4c285f3`.

## Cause

`candidate_authoring()` (#1440) sent every source with a pnpm/Yarn/Bun lock
to node-static/v2 ahead of npm. LibreChat declares `packageManager:
npm@11.13.0` and has `package-lock.json` plus `bun.lock`, so v2 refused it as
`preset_node_static_v2_bun_deferred` and the historical v1 route disappeared,
contradicting the v2 contract ("npm remains v1").

## Rule (pure `routes_to_node_static_v2`)

A. Any non-npm, malformed, or npm/non-npm-conflicting `packageManager` /
`devEngines.packageManager` → v2, which refuses ambiguity with a typed code
(never v1). B. Only npm declarations → v1. C. No declaration + npm lock → v1.
D. No npm lock + pnpm/Yarn/Bun lock → v2. E. Otherwise → existing v1.
An extra lock never takes the historical npm route away; an explicit author
declaration outranks lockfile heuristics.

## Evidence

- Fix `ff44ba75` on `461fa78c`. L0–L7 PASS (macOS and Linux aarch64);
  macOS selected Rust 729 PASS / 0 FAIL / 1 ignored.
- Cohort impact: of 50 pinned archives only LibreChat has an npm lock with
  another lock, and only its routing changes. Only LibreChat was rerun.
- LibreChat `40bb16ed…`, archive `1114382f…`, same policy (network denied,
  producer/decision off, 0 model calls), Linux aarch64 bwrap+landlock:

| | K | D | Terminal |
|---|---|---|---|
| 6a | `cf51a8d3…` | `ec2bd43c…` | `network_denied` |
| 50-wave (`c4c285f3`) | — | — | `preset_node_static_v2_bun_deferred` |
| after fix | `cf51a8d3…` | `ec2bd43c…` | `network_denied` (admission, no receipt) |

Restored exactly. Applied to the 50 wave this would move K-formed and
D-available 7 → 8; admitted, typed-K verified (3) and functional (0) are
unchanged. Not deployed; no remote migration.
