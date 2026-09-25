# Formation 2f — implementation starting point

## Baseline and scope

Base: `de4e4fc3645abfc5d2f6d0f62f2bfc0a95d9427b`, the merge of #1405.
The merge was pinned to reviewed head
`dc287b2b041b4a4657eeea1178567c8449462d89`; main was still
`da4d179059d2c9027058282aaeeeb75341c33a13`. Only that head was authorized
for admin bypass. No additional merge approval is implied for 2f.

2e is complete as an architectural migration, not as deployment or closure
of the Chrome baseline, upload-slot reclamation or environment capacity gates.
2f starts on `refactor/formation-minimal-execution-plan`. This document records
code inspection and the reduction boundary, **not an implemented migration**.

Target:

```text
Preset / authored input → AuthoringDraft → canonical D + I
                                              + Runtime binding
                                                    ↓
                                           minimal ExecutionPlan
                                                    ↓
                                           common realizer/executor
```

Keep Preset as the authoring frontend and the physical execution plan as the
binding boundary. Add no runtime capability, no fallback planner, and no
changes to K, receipt, lease/fence, state authority or network authorization.
All commits use `[skip ci]`. No deploy, flags, remote migration or manual CI rerun.

## Inspected current chain

1. `lib/runtime-attempt/src/plan.rs::plan_candidate` binds the draft, computes
   K/D refs, then calls `projection::project`, merges string overrides using
   provenance-dependent precedence, calls `compile_intent` and
   `compile_build_plan`, and appends projected authored build steps.
2. `lib/formation/src/projection.rs::project` writes lane/runtime/cwd/env/port/
   readiness/state and launch argv into string overrides. Argv is joined and
   quoted, and quote-containing elements are refused to protect the subsequent
   split. This is a legacy representation limit, not a reason to add shell
   evaluation to the replacement.
3. `lib/formation/src/intent.rs` reinterprets those values into
   `ProgramIntentV1`; `EffectiveBuildPlanV1` contains provisioned toolchains,
   physical build steps, workspace guest root and output-root binding.
4. `lib/runtime-attempt/src/executor.rs` consumes both intent and plan;
   static materialization consumes both as well. `ephemeral.rs` takes an entire
   intent but its process launch reads argv, cwd and public env.
5. Callers include local Formation, Hosted `job.rs`, and Runtime Network in
   `apps/formation-worker/src/runtime_network.rs`. Changing only the local
   entry point would leave two interpretations of D.

## Implementation checkpoints

- Establish baseline fixtures for authored process/static/generated-static,
  Python provisioning, preset frontend, and Runtime Network handoff. Compare
  K/D refs, argv element boundaries, cwd/env, ordered build steps, provisioned
  toolchains, ports, output roots and verification receipts. Execution IDs are
  volatile; semantic identities are not.
- Separate physical binding inputs (guest root, target triple, concrete
  toolchain/dependency locations, endpoint/state bindings) from canonical D
  fields. Preserve sandbox and input-identity checks.
- Bind typed execution fields directly from canonical D. Do not retain the
  join/split argv round trip under a renamed wrapper. Preserve authored build
  ordering and provisioning prerequisites; keep execution effects from D.
- Resolve preset/detector/caller authoring choices before D becomes canonical.
  Existing provenance-dependent post-bind override behavior must be covered
  explicitly rather than silently changing execution while retaining D's ref.
- Migrate common executor, temporary process realization and static output
  consumers together; inspect Hosted reporting of intent/plan digests before
  removing compatibility types or changing wire fields.
- Remove only dead reconstruction code after all callers have moved. Keep
  Preset and a minimal ExecutionPlan. Do not reduce IR by merely replacing
  type names or by deleting runtime-admission checks.

## Status

Repository and call-chain inspection started. No runtime code changed yet;
no 2f tests or completion claim. #1405's validation remains historical evidence,
not a test run of a future 2f implementation.


## Implementation update — 2f working branch

The preceding starting-point inspection is historical. The active implementation
now uses `bind_candidate → BoundCandidate { K, D, refs }`, then
`lower_execution(D, InputFacts, RuntimeBinding) → ExecutionPlan`.

`ExecutionPlan` holds candidate lane, canonical serving-step index, guest workspace
binding, resolved toolchain/package-manager bindings, ordered physical actions,
toolchain PATH and physical environment defaults. Authored actions are D step
indexes; argv/cwd/env/network/ports remain in D. Physical prerequisite commands
reuse the previous toolchain/dependency helpers, without generating either v1 IR.

Active `PlannedCandidate`, FormationRealizer, static/process lanes, Hosted v2 job
and Runtime Network requirements no longer consume `ProgramIntentV1`,
`EffectiveBuildPlanV1`, `intent_digest` or `plan_digest`. Historical codecs,
diagnostics and golden tests remain in `intent.rs` and test-only support modules.
Preset is still an AuthoringDraft frontend. ExecutionPlan is not a semantic ID.
Nonempty post-bind string overrides fail with `authoring_overrides_require_draft`;
callers must express those choices in AuthoringDraft, never execute a different
command under an unchanged D. Quoted/empty argv elements now reach the executor
as the canonical vector, with no command-string round trip.

### Regression evidence so far

- Before/after `lowering-before-2f.json`: seven authored fixtures (Static, Python,
  Node/npm, pnpm, yarn, authored exec, build+serve) preserve K/D refs, ordered
  physical commands, environment, toolchain PATH and Runtime requirements.
- Four preset comparisons against test-only baseline preserve K/D and build
  commands; SingleHtml/StaticFiles also preserve materialized manifest bytes/hash.
- macOS targeted formation/runtime-attempt/worker suite passed, including historical
  golden tests. Linux-only early returns are not counted as actual acceptance.
- Linux actual local tests: exec Formation 2, Hosted attempt fixture 6,
  Local Formation 18, Node Formation 4 passed; parity 2 passed. Log contains no
  reported skip. Hosted tests use a simulated artifact API, not deployed Hosted.
- Additional direct argv/override regression: local parity suite 3 passed.
- CLI + portable-application cargo check, targeted all-target clippy, fmt,
  diff whitespace and arch-check (43 packages) passed.

Actual Runtime Network acceptance is being run separately. This is not a 3d,
Stage 4, foundation completion, merged or deployed claim. No migration, flags,
workflow changes, CI reruns or deployments were performed.
