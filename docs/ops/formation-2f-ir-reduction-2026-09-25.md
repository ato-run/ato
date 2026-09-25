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
