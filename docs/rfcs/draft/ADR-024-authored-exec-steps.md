# ADR-024 — Authored `exec` steps in the EffectiveBuildPlan

**Status**: proposed

> **Current status (appended 2026-09-26).** Historical record of how authored `exec` steps first reached execution through the `EffectiveBuildPlan`. Since 2f (ato #1406, unmerged foundation stack) the active path lowers canonical D directly to `execution::ExecutionPlan`; authored steps are indexes into D (`BuildAction::Authored`), and `ProgramIntentV1` / `EffectiveBuildPlanV1` remain only as historical codecs for fixtures and diagnostics. The step semantics recorded here are unchanged.

**Context**: P0 Formation benchmark (`docs/ops/formation-p0-benchmark-2026-09-23.md`):
16 of 20 hand-written upstream Derivations use `ato.process@1` `op = "exec"`,
which the grammar has always accepted and the projection refused
(`projection_unsupported_step`). Code: `lib/formation/src/projection.rs`,
`lib/formation/src/authoring.rs`, `lib/formation/src/intent.rs`,
`apps/formation-worker/src/{job,build,sandbox,sandbox_exec}.rs`.

## The decision

### What a route this worker executes is

```
exec*   (authored order)   build / preparation — each runs to completion
serve   (exactly one)      realization
```

- An `exec` after the serving step is refused (`projection_step_order`): the
  execution model is build → materialize → realize, and a step after the
  realization has started has no place in it that means what it said.
- Two or more serving steps remain `projection_serving_steps` (no
  multi-service here).
- An `exec` is not a background service. A step that leaves a process
  behind has it stopped before the step returns (the build process-group
  invariant, #1387).

### What an `exec` step is (D semantics)

`id`, `argv` (an array, kept as written — never joined into a command line and
split again), `cwd`, `env`, and a new optional `network`:

```toml
[[derive.step]]
id = "install"
use = "ato.process@1"
op = "exec"
argv = ["..."]
network = "dependency-resolution"   # or "denied" (the default)
```

`network` states what the step **needs**; it is never inferred from the argv.
It is part of the `BoundStep` and therefore of the `DerivationRef`. `denied`
is the default and is never serialized, so a route that states nothing — and
every route formed before this field existed — keeps its `DerivationRef`
(`network = "denied"` written out is the same Derivation as leaving it out).
A serving step may not declare a network in v0.

Changing any of argv, cwd, env, network, or the order of steps changes the
`DerivationRef`. Nothing of the plan, the build host or its toolchain state
flows back into D (same D on two targets: two plans, one `DerivationRef`).

### Policy

The step declaration is the permission a step needs; the Formation policy is
the most any step may have. The narrower wins:

| step `network` | policy | outcome |
|---|---|---|
| denied | dependency-resolution | the step runs without a network |
| dependency-resolution | denied | refused before ANY step runs (local admission: `network_denied`) |
| dependency-resolution | dependency-resolution | the step has the network |

### The EffectiveBuildPlan (R-side projection)

```
platform prerequisites     e.g. provision the declared Python
authored exec steps        in D order
→ materialize → serve
```

An authored build **replaces** the application build the platform would
otherwise infer from the source; it is never added beside it:

- Python process: no detected `uv sync` / `pip install -r` is planned
  (`DependencyPlan::Authored`); the provisioned interpreter still is.
- Static (`ato.browser@1` serve): no detected package-manager build
  (`static.build = none`); a route with authored execs that also names a
  platform compiler or package build is refused as ambiguous.

`BuildStepV1` gains `cwd_relative` and `env`, omitted when empty: generated
plans digest exactly as before (golden test measured on the pre-change code).

### cwd

`""`/`"."` is `/app`; otherwise plain names only. Absolute paths and `..`
anywhere are refused at projection. Immediately before each step, the cwd is
resolved on the host against the real workspace (links followed) and must be
a directory inside it — an earlier step may have created it, or a link in its
place. The sandbox is handed that resolved guest path; nothing falls back to
the workspace root (`build_cwd_outside_workspace`).

### env

Authored env reaches the workload only. bubblewrap still starts from
`--clearenv` and its fixed build environment; the authored variables are
passed as `--env NAME=VALUE` flags to the `sandbox-exec` shim (before `--`)
and applied by the workload's own `exec`, after Landlock and no-new-privs are
in force. The shim never has them in its environment, so `LD_PRELOAD`,
`PATH`, `PYTHONPATH` and the like cannot affect what restricts the workload.
Names must be variable names; NUL is refused in names, values and argv.

## Not decided here

Node / Go toolchains and runtimes, pnpm / yarn, source transport beyond
32 MiB, multi-service, bindings, a Browser Contract for the static lane, and
the hosted resolver alignment of ADR-023. An exec can only use what the build
sandbox provides (system `/usr`, the provisioned Python, platform assets).
