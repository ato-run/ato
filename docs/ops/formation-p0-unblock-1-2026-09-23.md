# Formation P0 unblock 1 — acceptance (2026-09-23)

Two primitives from the P0 baseline (`formation-p0-benchmark-2026-09-23.md`),
then upstream searxng re-run unmodified:

1. **Bounded build output** — `build.rs` `run_step` drains stdout and stderr
   while the step runs (poll, non-blocking, 256 KiB tail per stream); the
   published diagnostic stays at 8 KiB.
2. **Contained symlink source semantics** — source resolver v2 (ADR-023):
   contained relative symlinks are tree entries (path + kind + target), the
   lowest covering resolver version is used (symlink-free trees stay v1, bit
   for bit), links are recreated as links on expansion and staging.

Found and fixed during acceptance (both blocked the acceptance itself):

3. The Runtime Network worker fetched a ticket's source (up to 32 MiB) under
   the client's 60 s timeout, so the ARM64 Runtime (~0.5 MB/s link) could not
   fetch searxng's 20 MB source (`source_unavailable: operation timed out`).
   The source fetch now allows 15 min.
4. A verified candidate whose artifact could not be stored lost its verdicts
   (`artifact_store_failed` with no verification): the verdicts are kept.

## Setup

ato `feat/formation-p0-unblock-1`, ato-api main `11416294` under
`wrangler dev --local` (local D1/R2). Runtimes as in the baseline:
`rt_sugamo-x86` (Ubuntu 26.04, x86_64, bwrap, contained browser, Chrome 145
`chrome-no-sandbox`), `rt_oci-arm64` (Ubuntu 24.04, aarch64, bwrap, contained
browser, Chrome 149), `rt_mac-m1` (no containment).

Source: `searxng/searxng@2ed96e6fcfc96ca1045155fc52a12f5f7b070417`, fresh
checkout, **unmodified** — including the symlink
`utils/templates/etc/apache2 -> httpd`. Route:
`benchmarks/formation/p0/searxng/capsule.toml` (the baseline's, unchanged).

## searxng

| Run | Runtime | Result | HTTP K (`/healthz`, `/`, source identity) | Browser K | Attempt time |
|---|---|---|---|---|---|
| K0, `all` (before fix 3/4) `01M36MD50BE4ZZPZ7D3GHQZQM8` | x86_64 | `artifact_store_failed` (verdicts not recorded — fix 4) | passed (the store step runs only after verification) | — | 43 s |
| | aarch64 | `source_unavailable` — source fetch timed out at 60 s (fix 3) | — | — | 63 s |
| **K0, `all` (diagnostic retry)** `01M36MQTRXEB3HV4D3NGVRS1XY` | **x86_64** | `artifact_store_failed` | **satisfied ×3 — Level 3** | — | 46 s |
| | **aarch64** | `artifact_store_failed` | **satisfied ×3 — Level 3** | — | **59 s** |
| K1 (Browser Contract, once) `01M36MWG6WQTR6F283JQY1EDCA` | x86_64 | `browser_contract_inconclusive` | satisfied ×3 | **inconclusive — `agent_provider_refusal`** (DeepSeek: "Content Exists Risk") | 45 s |

Identity: D `sha256:65eaf36b…` on both architectures; base K
`sha256:ae9e1150…`; effective K with the Browser Contract `sha256:d7858adc…`.
The source is measured under resolver **v2** (it has a symlink).

- The requester no longer refuses the source: the P0 baseline's first
  blocker for searxng (`source_layout`) is gone, and the Coordinator issued
  tickets to both Linux Runtimes; the Mac was filtered (process containment).
- ARM64's dependency install, which hung on an undrained pipe until the
  15-minute budget in the baseline counterfactual, completed; the whole ARM64
  attempt took 59 s.
- **Highest level: 3 on x86_64 and on aarch64** (typed K PASS).
- **Next real blockers**:
  - Level 5: the verified workspace carries the source's symlink, and the
    deterministic artifact format (`pack_tree`) has no symlink entry —
    `artifact_store_failed` (`publish` stage). Extending the artifact format
    is a separate decision (Runners consume it).
  - Level 4: the browser agent model (DeepSeek flash) refuses the searxng
    page, as in the baseline (third time). Model-side, not the application:
    the typed K passed in the same attempt. No automatic model fallback was
    added.
  - Detected (not changed): the `artifact_store_failed` message carries the
    worker's host scratch path (`/home/…/work/…/workspace/…`).

## Tests

| Suite | macOS | OCI aarch64 | sugamo x86_64 |
|---|---|---|---|
| `ato-formation` lib (96, incl. 10 new resolver v2 tests) | pass | pass | pass |
| `build` unit (7 new: 4.5 MB stdout / stderr / both, failure keeps last words, typed failure after megabytes, never-ending step and its group stopped, descendant holding pipes) | pass | pass | pass |
| `source_symlinks_v1` (6 new: local = archive v2 closure, symlink-free stays v1, escaping links refused through a local snapshot, staging recreates links, contained build reads through a link, no link reaches a host canary from the build sandbox) | pass (sandbox cases skip: no bwrap) | 6/6 | 6/6 |
| `local_formation_v1` (+1: verified candidate keeps verdicts when the artifact cannot be stored) | pass | 15/15 | serial 14/14 ¹ |
| `runtime_network_v1`, `browser_verifier_protocol_v1`, `browser_verifier_containment_v1`, `temporary_realization_v1`, `static_lane_v1`, `pack_v1`, `authoring/intent/preset` | pass | pass | pass |
| `sandbox_v1` | pass (skips) | 10/11 ² | 10/11 ² |

¹ On sugamo, `local_formation_v1`'s two process-realization tests fail
intermittently when run in parallel (`server_bind` on the candidate port).
The same happens on main (2 of 5 runs), so it is pre-existing; serially 4/4.
² `a_step_that_declared_no_network_does_not_get_one` looks for the worker
binary beside the test binary; pre-existing, unchanged from main.

Also fixed: `browser_verifier_containment_v1`'s cleanup test compared scratch
listings outside the lock the other tests verify under (a parallel test's
live scratch read as a leftover once on x86_64).

## Not done here

Authored `exec` projection, Node / Go runtimes and toolchains, pnpm / yarn,
>32 MiB Runtime Network transport, multi-service, Java / PHP, bindings,
submodules / LFS, static browser realization, agent model fallback, the
artifact-format symlink entry, control-plane resolver version for hosted
sources. No deploy, no migration.
