# B1_BASELINE — consolidated nightly

Recorded 2026-09-03 at the end of **I0 — Nightly Baseline Consolidation**.

Everything Personal / Dynamic Compute had accumulated across a stack eleven
Draft PRs deep now lives on nightly, and the Static materializer that existed
only on `main` lives there too. B1-B onwards branches from here, not from a PR.

## The baseline

| Repo | nightly SHA |
|---|---|
| `ato-run/ato` | `bb1d4b9011f61c89d1e71c6bb84212f57425a6f1` |
| `ato-run/ato-api` | `c4d30f942b0781719f693ce9b8fa6290f6360904` |
| `ato-run/ato-pwa` | `0c1785e0f1f0905eff03f776fb62ff4565143ac3` |

Static donor: `ato` `main` @ `e2ce186c7a51c2e71a4fde0173e054dde2354d91`.

Quote these three SHAs as `B1_BASELINE` in every B1 PR body.

## What was merged, in dependency order

**ato** — #1330 (P3.1) → #1331 (P3.2) → #1332 (P3.3) → #1333 (P3.5) →
#1334 (P3.6) → #1335 (P3.7) → #1337 (Static integration) → #1336 (B1-A).

**ato-api** — #545 (Compute model) → #548 (P2 state) → #549 (P3 dispatch) →
#550 (P3.6 authorization) → #551 (P3.7 worker wiring) → #552 (B1-A).

**ato-pwa** — #314 (Personal Apps), whose head was still the exact commit its
staging acceptance ran against.

Merge commits throughout. Each child was retargeted to nightly only after its
parent landed, and its diff re-checked to be its own slice and nothing more.

## Deliberately NOT merged

**ato#1327** — the P0 bridge branch. It touches 191 files, **154 of them under
`apps/desktop`**, with ~38k deletions. Merging it to obtain six static-web files
would have dragged a desktop refactor into the baseline. The six files were
transplanted instead (#1337).

**ato#1328** — the old Builder daemon branch, based on
`deploy/replay-static-lane`. Kept as deployed legacy evidence until the B1-F
Static Formation cutover, then closed as superseded. **No old daemon source is
on nightly.**

`tools/snapshot-builder/src/bin/static-web-bundle.rs` — belongs to the old
builder topology; the materializer builds without it.

COOP, Desktop, Activity and the nightly→main promotion PR are untouched.

## Static integration

Per-file transplant onto a branch cut from nightly, not a `main` merge:

    main   -> extensions/materializers/static-web, verbatim (16 files)
    #1327  -> the P0 Browser Instance State instrumentation, and nothing else

Identity was checked rather than assumed. The manifest JCS, receipt JCS and
frame-ancestors fixtures are **byte-identical to main's**, so existing artifacts
keep their digests and keep opening. The only source difference from main is the
P0 instrumentation. No second `static_web_*` implementation exists, and no Run
or Runner dependency entered the Static lane.

## Migration reconciliation

`0195`–`0200` were absent from nightly, so nothing collided. The canonical file
names match what staging already recorded, name for name:

    0195_personal_apps_compute_instances_v0
    0196_dynamic_instance_state_v0
    0197_compute_instance_runs
    0198_compute_instance_run_states
    0199_run_workspace_and_reclamation
    0200_lease_ready_local_port

`wrangler d1 migrations list` on staging: **no migrations to apply**. Nothing
was renumbered, so the tree and the database still tell the same story.

## Staging deployment

| | |
|---|---|
| `ato-store-staging` version | `b09d72a9-3b5a-4dfb-81c4-c10ca4ce2f84` |
| Worker source | `ato` nightly `bb1d4b9` |
| Worker binary SHA-256 | `2627d343d1bde45af62191d7ad99fc90a1838257565fa18055257b81724c27e7` |
| Runner | `01M1JVXA4DX08ZR0VJQXV3BZFA` on `ubuntu-sugamo`, 1 slot |
| Production | **unchanged** |

## Tests at the baseline

| | |
|---|---|
| `ato` | `cargo check --workspace` clean; 179 focused tests pass (worker 80, ipc 92, sandbox 19, static-web 25) |
| `ato-api` | `tsc --noEmit` clean; 95 tests pass across six suites |
| `ato-pwa` | 1768 tests pass across 197 files |

## P3 Scenario A–H, re-run on the consolidated nightly

Not a replayed receipt — every scenario was executed again against the new
worker binary and the new staging deployment.

| | Scenario | Result | Evidence |
|---|---|---|---|
| A | cold start | **PASS** | fence 9, restored `first, second` on startup |
| B | write + commit | **PASS** | `consolidated` written over HTTP; revision `…97Y5S5WR1P`, parent `…D1PVB56HR` |
| C | fresh wake | **PASS** | fence 10, different PID and port, all three notes present |
| D | third Run | **PASS** | fence 11, different PID/port/workspace again, state continuous |
| E | stale writer | **PASS** | `409 writer_fence_stale`, `409 state_parent_revision_mismatch`, head unmoved |
| F | readiness failure | **PASS** | early exit caught by its exit, not by timeout; grant `aborted`, no revision |
| G | auth + sandbox | **PASS** | 401 / 404 / 404 / 404 refusals; `/app` read-only, host sentinel invisible, nothing escaped to the host |
| H | worker crash | **PASS** | SIGKILL took the workload with it, no release sent, fenced reclamation freed the slot, fence 14 recovered, dead lease refused, head never moved |

## B1-A Formation contract at the baseline

Rust 14 tests, TypeScript 13 tests, cross-language canonical digests equal
(`expected-digests.json` asserted by both sides).

## Open PRs after consolidation

`ato` #1328 (held as legacy evidence). Unrelated COOP / Desktop / Activity PRs
untouched. `ato-api` #530 (nightly→main promotion), #543, #542, #526, #544
untouched.

## B1-B is cleared to start

From `ato` `bb1d4b9` and `ato-api` `c4d30f9`, as fresh branches. Not stacked on
B1-A or on any P3 branch.

`source_revisions` and `source_materializations` are the source-of-truth model
and are reused; `source_closure_ref` maps onto the existing
`source_tree_digest` + `resolver_contract_version`. No parallel closure model is
created.
