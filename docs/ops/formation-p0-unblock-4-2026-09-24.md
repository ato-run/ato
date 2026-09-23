# Formation P0 unblock 4 — generic Process + Node / npm / pnpm / yarn (2026-09-24)

Base: ato `main` `7824422e` (#1387 merged as `2992101f`, #1388 as
`7824422e`). Branch `feat/formation-p0-unblock-4-generic-process-node`. Local
Coordinator: ato-api `d4800415` under `wrangler dev --local` (local D1, no
remote migration). No deploy. Every commit `[skip ci]`. Semantics:
`docs/rfcs/draft/ADR-025-generic-process-node-runtime.md`.

## What changed

1. **Generic `process` lane.** A process route that declares a runtime
   other than Python compiles to `Lane::Process` (wire `process`); there is
   no Node lane or executor. Routes declaring only Python (or nothing) stay
   on `python_process`, byte for byte.
2. **Declared runtimes, exact.** Node `20.20.2` / `22.14.0`, Python from the
   catalog, several at once (python + node). Never inferred from the argv.
3. **Package managers.** npm = the declared Node's. pnpm / yarn from the
   source's exact `packageManager`, else a `[[runtime]] pnpm|yarn` in the
   Derivation; a pnpm/yarn lockfile or unpinned `packageManager` with
   neither → `package_manager_version_unresolved`. Installed into
   `/opt/ato/toolchains/<manager>/<version>` with the declared Node's npm,
   reported version checked.
4. **Toolchain visibility.** The plan's `toolchain_path` heads the build
   sandbox PATH (so `sh -c "npm …"` finds the declared ones); the intent's
   PATH does the same for the Run, over the host PATH the Runtime launch
   would otherwise pass through. `HOME=/tmp` and `npm_config_cache` defaults
   for the Run; package-manager caches never land in the workspace.
5. **Runtime Network.** A generic process requires `runtime.process`; Node,
   pnpm, yarn are `toolchain.<name>.<version>` provisions (advertised by the
   worker when present).
6. **Operator log.** An attempt failure's cause (head and tail, bounded) is
   written to the Runtime's own log; the requester still gets the typed code
   and one sentence.

## Completion conditions

| # | Condition | Evidence |
|---|---|---|
| 1 | Node as generic Process, no language executor | `process_toolchains_v1::a_node_route_is_a_generic_process_with_its_declared_node` (lane `process`, requirement `runtime.process`, provision `toolchain.node.22.14.0`, no `runtime.node`); same TemporaryRealization / Runtime process boundary (`executor: runtime-process` in every realization record) |
| 2 | existing Python identities unchanged | `plan_identity_golden_v1` 5/5: pip, uv, static (pre-exec code), Python + exec and static + exec (measured on main `7824422e`) — DerivationRef, intent and plan digests |
| 3 | exact Node in build and serve | `node_formation_v1`: build records `process.version = v22.14.0`, `execPath = /opt/ato/toolchains/node/22.14.0/bin/node`; the server answers `/health` 200 only when it runs on the same, and typed K passed |
| 4 | no host Node/npm fallback | hosts carry their own Node (OCI `/usr/bin/node` v18.19.1, sugamo v22.22.1); every build and Run reported v22.14.0 from the toolchain root |
| 5 | npm in build and Run | `npm_builds_and_starts_on_the_declared_node`: `sh -c "npm run build"` (user agent `node/v22.14.0`), the package's `prebuild` lifecycle script on the same Node, `npm start` serving |
| 6 | exact pinned pnpm / yarn, no host-global | `an_exact_pnpm_pin_builds_and_serves` (pnpm 9.15.4), `an_exact_yarn_pin_builds_and_serves` (yarn 1.22.22): provisioned into the toolchain root, `pnpm run build` / `yarn run build` and `… run start`; offline plan tests for yarn berry (`@yarnpkg/cli-dist`), Corepack hash suffix, D pin, source/D conflict, ranges, missing pins |
| 7 | Python + Node route projects | `a_python_and_node_route_provisions_both_and_serves_python`: provision python → provision node → authored exec → serve python |
| 8 | network default-deny | unchanged rule (ADR-024 tests); the synthetic and fixture routes declare no step network — only the platform's provisioning uses the policy |
| 9 | synthetic Node VerifiedRoutes x86_64 + ARM64 | below |
| 10 | Uptime Kuma / Homepage tried, next blockers recorded | below |
| 11 | P0 D-gate Node blocker 8 → 0 | below |
| 12 | clean target directories | every Linux run below used a per-tree, per-host `CARGO_TARGET_DIR` |

## Synthetic Node app on the Runtime Network

`node build.js` (exec) → `node server.js` (serve), `[[runtime]] node =
22.14.0`, K = `/health` 200 + source identity. `--mode all`.

- Satisfy `01M383W30YAK84FT2WM4YBZZEM` → **satisfied**.
  - K `sha256:6e9c6f0d4f493f7e076d460541a3aebfe5954db37e4f1aed6a9a447cc28a6124`
  - D `sha256:d1d348fa2b8844ee3f7b502d56eac2998a5e2f91f4bb24bddd54ab278b73ee4b`

| | Linux aarch64 `rt_oci-arm64` | Linux x86_64 `rt_sugamo-x86` |
|---|---|---|
| Attempt | `01M383W31BZZ019Z74705JGGYG` pass | `01M383W4HTMB44MN22YXEEF39F` pass |
| Typed K | health, source-identity satisfied | same |
| Artifact | `sha256:9d2255c43768cc40f69518702977d9326dd7f956d4c10abbe18f90f50707e9d1` | `sha256:94aaf7c598fdd7e7e0b51ce3b8929cfcf11a1a1b9226ee9ba7d118f1da370b9e` |
| `out/build.json` in the artifact | `v22.14.0`, `/opt/ato/toolchains/node/22.14.0/bin/node`, `arm64` | `v22.14.0`, same path, `x64` |
| Capability profile | `sha256:418cacda…` | `sha256:3a28c616…` |
| **VerifiedRoute** | **`01M383W4H6K0394XDP66W8E0EK`** | **`01M383W620KV4E2DQKBQNJKBDA`** |

Same K and D, two VerifiedRoutes. macOS Runtime not started (not a process
Runtime). Realization: `runtime-process`, `bwrap+landlock`, candidate
network `no-egress; tcp bind limited to allocated host ports`.

## P0 apps

### Uptime Kuma — `louislam/uptime-kuma@e62702d868a0b072c6e1870e5278762319637f04`

Route: the baseline D with only `network = "dependency-resolution"` added to
`npm ci` and to `npm run download-dist` (the latter downloads the release
`dist.tar.gz` from GitHub). Source unmodified. Node `20.20.2` as in the
baseline D.

- Honest attempt: satisfy `01M383XGZXM8R94AJCNB0QEP3C` → **unsatisfied**
  - ARM64 `01M383XH16TNAAB23REGKKFCT7` failed at build (`formation_failed`).
  - x86_64 `01M383YH5Q33147TXVM6VE7688` failed at build (`formation_failed`).
- Diagnostic runs, not counted: satisfy `01M3845VJ8BW2Q2EGBJKEE13Z0` (both Runtimes) and `01M384ANA2CZPK2N10EHTJ80N9` (x86_64 only), with the operator log added.
  - The `npm ci` step fails installing `better-sqlite3@12.11.1`: `prebuild-install` finds no binary for Node 20.20.2.
  - The `node-gyp` fallback then runs `make`, which fails with `cc: No such file or directory`. In the build sandbox, `/usr/bin/cc` → `/etc/alternatives/cc` does not resolve, because `/etc` is bound file by file.
  - The same `npm ci` outside the sandbox (diagnostic) compiles and succeeds.
  - ARM64 fails at the same step with the same missing prebuild. Its `cc` line was cut off in the head-only log of the first diagnostic run; the cause was confirmed on x86_64 with head and tail logging.
- **Next blockers:**
  1. `native_toolchain_missing`: no C compiler reachable in the build sandbox.
  2. `runtime_version`: upstream `engines.node >= 26.2.0`, and its start script uses `node --run`, but the baseline D declares 20.20.2 and the catalog has 20.20.2 and 22.14.0 only.
  3. `download-dist` finds no `dist.tar.gz` for `3.0.0-beta.0` (prints "dist not found", exit 0). The frontend would then be missing, and `vite build` would be needed.
- The Node blocker itself is gone: Node 20.20.2 was provisioned and used, and npm ran under it with its network.

### Homepage — `gethomepage/homepage@d1c1ee7a1ef0ab3d749fa59bed054632f26710dc`

Route: the baseline D with `network = "dependency-resolution"` on
`pnpm install`. Node 22.14.0.

- Refused at requester planning, before submission: **`package_manager_version_unresolved`**.
  - The source carries `pnpm-lock.yaml` (lockfile v9.0) and no `packageManager`.
  - Upstream's Dockerfile uses `corepack prepare pnpm@latest`, so no exact pnpm version exists upstream.
- No version was chosen. A Derivation could pin one with `[[runtime]] pnpm`, but that is an authoring decision this benchmark does not make on the app's behalf.
- **Next blocker:** `package_manager_version_unresolved`.

## P0 D-gate re-run

Same 20 Ds, empty Initial Condition, unreachable API (nothing submitted):

| First blocker | After #1388 | Now |
|---|---|---|
| runtime `node` not provisioned | 8 | **0**: all 8 plan and reach submission |
| runtime `go` not provisioned | 6 | 6 |
| runtime `java` not provisioned | 2 | 2 |
| two serving steps | 3 | 3 |
| `intent_no_lane` (searxng, empty I) | 1 | 1 |

The 8 that now plan: open-webui (python + node), uptime-kuma, excalidraw
(static + node build), nocodb, n8n, node-red, code-server, homepage. Against
their real sources they meet their next blockers, most commonly:
- source transport beyond 32 MiB (open-webui, excalidraw, nocodb, n8n, node-red);
- source layout: Git LFS and a submodule (code-server);
- package manager version (homepage);
- native toolchain and Node version (uptime-kuma).

## Tests

New suites:

| Suite | macOS | OCI aarch64 | sugamo x86_64 |
|---|---|---|---|
| `process_toolchains_v1` (13, offline plan tests) | pass | pass | pass |
| `node_formation_v1` (4: node, npm, pnpm 9.15.4, yarn 1.22.22 — built and served on Node 22.14.0; worker env canary absent; host home invisible; lifecycle script contained) | skips (no bwrap) | 4/4 ×2 | 4/4 ×2 |
| `plan_identity_golden_v1` (5) | pass | pass | pass |

Full suites (`ato-formation`, `ato-formation-worker`,
`ato-portable-application`; `ato-connected-realization-worker` lib on OCI).
Each tree had its own `CARGO_TARGET_DIR` on each host (`p0u4-target` for the
branch, `p0u4-base-target` for base).

| | Branch (2 runs per host) | Base `7824422e` (2 runs per host) |
|---|---|---|
| macOS | 384 passed, 0 failed | — |
| `sandbox_v1::a_step_that_declared_no_network_does_not_get_one` | fails every run, both hosts | fails every run, both hosts |
| `local_formation_v1` process realizations (parallel port race) | OCI run 2: 1 | OCI run 1: 1; sugamo: 1 per run |
| `ato-portable-application` `local_static_server_rejects_browser_state_without_the_run_token` | — | sugamo run 2: 1 (flaky) |
| `ato-connected-realization-worker` lib, OCI | 34 failed | 40 failed |

Every branch failure also occurs on base. No failure is introduced by this
change. The connected-realization-worker lib failures are
environment-dependent and were not investigated here; this branch does not
change that crate.

## Cleanup

- Runtime workers stopped on both hosts. Tunnels and `wrangler dev` stopped. Runner tokens deleted.
- Runtime work and out directories removed, after the evidence above was taken. The diagnostic Uptime Kuma copies on sugamo were removed.
- Base comparison trees and their build directories removed.
- The toolchains provisioned into `/opt/ato/toolchains` are kept on both hosts. They are the shared, read-only toolchain root this feature uses:
  - Node 22.14.0
  - pnpm 9.15.4
  - yarn 1.22.22
