# Formation P0 unblock 3 — authored `exec` projection (2026-09-23)

Base: ato `feat/formation-p0-unblock-2` head `feb839e6` (#1387: main `c0de5efb`
+ the process-group and artifact-symlink work; #1387 not yet merged at the
time of writing, so this branch is stacked on it). Branch
`feat/formation-p0-unblock-3-exec`. No deploy, no migration, every commit
`[skip ci]`. Semantics: `docs/rfcs/draft/ADR-024-authored-exec-steps.md`.

## What changed

1. **`exec* serve`** — zero or more `ato.process@1` `exec` steps in authored
   order, then exactly one serving step, now project into the
   EffectiveBuildPlan. An `exec` after the serve → `projection_step_order`;
   two serves → `projection_serving_steps` (unchanged).
2. **Step network in D** — `network = "denied" | "dependency-resolution"` on
   an `exec` step, carried in `BoundStep`; `denied` is the default and is not
   serialized. Refused on a serving step.
3. **Plan composition** — platform prerequisites (provisioned Python) first,
   then the authored steps. An authored build replaces the inferred
   application build: no detected pip/uv install (`DependencyPlan::Authored`),
   no detected npm build for a static route; authored execs plus a named
   platform/package build is refused.
4. **`BuildStepV1.cwd_relative` / `env`** — omitted when empty.
5. **Sandbox** — authored env and cwd go to the `sandbox-exec` shim as flags
   and are applied by the workload's `exec`, after Landlock; bubblewrap keeps
   `--clearenv` + its fixed environment. The cwd is resolved on the host
   against the real workspace right before its step and must stay inside
   (`build_cwd_outside_workspace`). A step needing more network than the
   policy allows is refused before any step runs.
6. **#1387 final hardening** (pushed to #1387 as `feb839e6`, before this
   branch): a failed `child.wait()` goes through the same stop-and-confirm
   path as every other exit; a `waitid` error is not read as "running";
   `kill(-pgid, 0)` = `EPERM` counts as alive, only `ESRCH` as gone.

## Merge conditions

| # | Condition | Evidence |
|---|---|---|
| 1 | `exec* + one serve` projects | `exec_projection_v1::exec_steps_run_in_authored_order_after_the_platform_prerequisites`, projection unit tests |
| 2 | authored order / argv / cwd / env exact | same test: `["…python3","gen.py","a b",""]` stays 4 elements; plan names `provision-python, install, generate` |
| 3 | step network in D identity, enforced by policy | `every_part_of_an_exec_step_is_part_of_the_derivation`, `a_network_stated_as_the_default_is_the_same_derivation`; `exec_build_v1` A/B/C (below) |
| 4 | authored env does not reach the shim | `authored_env_is_given_to_the_workload_and_never_to_the_sandbox` (no authored name in any `--setenv`; `--env` only after `sandbox-exec`; next step does not see it) |
| 5 | cwd cannot escape | projection refuses `/tmp`, `/etc`, `..`, `../x`, `a/../../x`, `a/../b`; at run time a link to `/etc`, a climbing link and a missing dir are refused before the step runs, with no host path in the message |
| 6 | process-group invariant on build steps | every step still goes through `run_step` (#1387) |
| 7 | existing D/plan identity unchanged | `plan_identity_golden_v1`: DerivationRef, intent digest and plan digest of a pip route, a uv route and a static route equal the values measured on the pre-change code |
| 8 | process fixture end to end → VerifiedRoute | `exec_formation_v1::a_process_route_with_exec_steps_reaches_a_verified_route` |
| 9 | static fixture typed K PASS | `exec_formation_v1::a_static_site_an_exec_step_builds_satisfies_its_contract` |
| 10 | P0 D-gate exec refusals 16 → 0 | below |

### Network (merge condition 3), no internet involved

A loopback listener on the host; a step dials it with bash `/dev/tcp`.

| Step declares | Policy | Result |
|---|---|---|
| (nothing = denied) | dependency-resolution | no network: dial refused (`Connection refused` asserted, so the probe itself ran) |
| dependency-resolution | denied | refused before any step runs; the step BEFORE it did not run either |
| dependency-resolution | dependency-resolution | dial reached the listener |

While writing these, a first version of the probe redirected stderr to
`/dev/null`, which Landlock makes read-only in the build: the redirect failed
and every dial looked unreachable, so the "denied" case passed vacuously. The
probe now writes stderr into the workspace and the denied case asserts the
refusal itself.

### Fixtures

- **Process**: `exec configure` (root; creates `generated/config.txt`) →
  `exec render` (`cwd = "generated"`, `EXPECTED` env, argv element
  `"rendered by exec.txt"`; checks cwd and env, writes the runtime file) →
  `serve` Python HTTP → typed K (`/health` 200, source identity) → artifact →
  VerifiedRoute. The artifact holds `generated/config.txt`,
  `generated/rendered by exec.txt`, `site/health.txt`, and not
  `runtime-created.txt`, which the candidate writes during realization.
- **Static**: `exec build` writes `dist/index.html`; `ato.browser@1` serves
  `root = "dist"`; typed K `GET /` 200. The source also has a `package.json`
  with a `build` script and a lockfile. That build is not run, and no Node is
  provisioned.

## Tests

New tests (all parallel-safe; one process realization per test binary):

| Suite | macOS | OCI aarch64 | sugamo x86_64 |
|---|---|---|---|
| `ato-formation` lib (98, incl. projection unit tests) | pass | pass | pass |
| `exec_projection_v1` (11) | pass | pass | pass |
| `plan_identity_golden_v1` (3) | pass | pass | pass |
| `exec_build_v1` (6) | skipped (no bwrap) | pass ×3 | pass |
| `exec_formation_v1` (2) | Filtered (asserted) | pass (Formed) | pass (Formed) |
| `build::` process-group tests (10, #1387 hardening) | pass | pass ×3 | — |

Full suites (`ato-formation`, `ato-formation-worker`, `ato-portable-application`,
and `ato-connected-realization-worker` on OCI) were run twice per Linux host.
Each tree had its own build directory. An earlier round shared one
`CARGO_TARGET_DIR` between two source copies, which made cargo link the other
copy's crates; those results were thrown away and rerun. The only failures
are pre-existing, and each was reproduced on the base (`feb839e6`, own build
directory):

- `sandbox_v1::a_step_that_declared_no_network_does_not_get_one`: every run
  on both hosts, branch and base (harness binary-path assumption, known).
- `local_formation_v1` process-realization tests (port race when run in
  parallel): 0–3 per run, branch and base (base: 3/3 runs on sugamo).
- `ato-connected-realization-worker` lib, OCI: 5–28 failures per run in
  volume / network_broker / recovery / session / HTTP gate tests. Base shows
  the same modules failing (24). Environment-dependent; not investigated here.

## P0 D-gate re-run

Same probe as the baseline: each of the 20 hand-written Ds, unchanged, as
`--route` against an intentionally empty Initial Condition, `ato form
--runtime-network --mode all --network dependency-resolution`. The requester
plans before contacting the Coordinator; the API was pointed at an
unreachable address, so nothing was submitted and nothing ran. Ran on macOS
(requester planning is host-independent).

| App | Baseline | Now |
|---|---|---|
| open-webui | projection_unsupported_step (exec) | runtime `node` not provisioned |
| uptime-kuma | projection_unsupported_step (exec) | runtime `node` not provisioned |
| gitea | projection_unsupported_step (exec) | runtime `go` not provisioned |
| memos | projection_unsupported_step (exec) | runtime `go` not provisioned |
| Stirling-PDF | runtime `java` not provisioned | runtime `java` not provisioned |
| excalidraw | projection_unsupported_step (exec) | runtime `node` not provisioned |
| nocodb | projection_unsupported_step (exec) | runtime `node` not provisioned |
| metabase | projection_unsupported_step (exec) | runtime `java` not provisioned |
| n8n | projection_unsupported_step (exec) | runtime `node` not provisioned |
| node-red | projection_unsupported_step (exec) | runtime `node` not provisioned |
| grafana | projection_unsupported_step (exec) | runtime `go` not provisioned |
| immich | projection_serving_steps (2) | projection_serving_steps (2) |
| navidrome | projection_unsupported_step (exec) | runtime `go` not provisioned |
| FreshRSS | projection_serving_steps (2) | projection_serving_steps (2) |
| searxng | intent_no_lane (empty I) | intent_no_lane (empty I) |
| paperless-ngx | projection_unsupported_step (exec) | projection_serving_steps (2) |
| code-server | projection_unsupported_step (exec) | runtime `node` not provisioned |
| portainer | projection_unsupported_step (exec) | runtime `go` not provisioned |
| homepage | projection_unsupported_step (exec) | runtime `node` not provisioned |
| filestash | projection_unsupported_step (exec) | runtime `go` not provisioned |

- **`projection_unsupported_step` for `exec`: 16 → 0.** Every `exec` step of
  those 16 Ds is projected, and passes the argv/env/cwd checks. The checks
  run before the runtime check, so none of them is hidden behind it.
- **Next blockers** (not addressed here):
  - Node not provisioned: 8
  - Go not provisioned: 6
  - Java not provisioned: 2
  - two serving steps: 3 (paperless-ngx was hidden behind its exec refusal before)
  - searxng's `intent_no_lane` is an artifact of the empty Initial Condition, as in the baseline.
- **For the next phase:** the 20 Ds were written before `network` existed, so their
  install steps (`npm ci`, `go build` fetching modules, `pip install`) now
  default to `denied`. When Node/Go toolchains make them runnable, those Ds
  need `network = "dependency-resolution"` on the steps that fetch. The Ds
  are kept as in the baseline here.

## Limits and follow-ups

- **What an exec can use:** only what the build sandbox provides. That is system `/usr`, the provisioned Python and the platform assets. Node / Go / Java / pnpm / yarn are the next phase.
- **Hosted worker path:** the hosted Formation job uses the same
  `plan_candidate`, so it also projects `exec` routes now. The existing
  preflight still refuses a publish-enabled job that needs the network. Nothing
  was deployed.
- **Static lane Browser Contract:** still not realized. Out of scope.
- **Pre-existing test failures, reproduced on base; not fixed:** see Tests.
