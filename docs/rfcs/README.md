# Current RFCs

Only documents under `accepted/` in this directory are current normative Ato
architecture. Pre-computation RFCs were moved to `docs/archive/` and are
historical evidence, not implementation authority.

- [Computation Architecture](accepted/COMPUTATION_ARCHITECTURE.md)
- [Object Closure Bundle](accepted/OBJECT_BUNDLE.md)
- [Composition](accepted/COMPOSITION.md)
- [Local Capsule Repository](accepted/LOCAL_CAPSULE_REPOSITORY.md)
- [Protocol Adapter](accepted/PROTOCOL_ADAPTER.md)
- [Materialization](accepted/MATERIALIZATION.md)
- [Capsule Bundle](accepted/CAPSULE_BUNDLE.md)
- [Capsule CLI Lifecycle](accepted/CAPSULE_CLI_LIFECYCLE.md)

## Implementation-track drafts (non-normative)

Documents under `draft/` are non-normative: they are not implementation
authority, whether they describe merged code or pending plans. Draft status
alone says nothing about implementation state — some drafts describe merged
behavior, others describe work not yet started. Check the roadmap state and
the merged implementation before relying on any draft. Adopted spec and
merged implementation are separate states.

- [Formation roadmap](draft/FORMATION_ROADMAP.md) — the single progress
  reference for the Formation stages. Its stage states (`Merged` / `Pending` /
  `not deployed` / `deploy status unconfirmed`) are the authority; do not copy
  the stage table into other files. Pending rows are plans, not shipped
  functionality.
- Formation attempt, verification, and receipt authority (draft):
  [ADR-018](draft/ADR-018-formation-build-sandbox-substrate.md),
  [ADR-019](draft/ADR-019-formation-ephemeral-verification.md),
  [ADR-026](draft/ADR-026-formation-result-states-and-model-judged-observations.md),
  [ADR-027](draft/ADR-027-runtime-attempt-layer.md),
  [ADR-029](draft/ADR-029-coordinator-receipt-authority.md),
  [ADR-030](draft/ADR-030-hosted-formation-common-attempt.md)
- Portable application bundle and authoring (draft):
  [ADR-017](draft/ADR-017-legacy-formation-adapter.md),
  [ADR-024](draft/ADR-024-authored-exec-steps.md),
  [ADR-025](draft/ADR-025-generic-process-node-runtime.md),
  [PORTABLE_APPLICATION_BUNDLE_V3](draft/PORTABLE_APPLICATION_BUNDLE_V3.md),
  [PORTABLE_APPLICATION_AUTHORING_V2](draft/PORTABLE_APPLICATION_AUTHORING_V2.md),
  [PORTABLE_LOCAL_INSTANCE](draft/PORTABLE_LOCAL_INSTANCE.md),
  [PORTABLE_INSTANCE_SNAPSHOT](draft/PORTABLE_INSTANCE_SNAPSHOT.md),
  [PORTABLE_DEPENDENCY_EXPORT](draft/PORTABLE_DEPENDENCY_EXPORT.md),
  [PORTABLE_DYNAMIC_DERIVATIONS](draft/PORTABLE_DYNAMIC_DERIVATIONS.md),
  [PORTABLE_OCI_SERVICE_GROUP](draft/PORTABLE_OCI_SERVICE_GROUP.md),
  [formation-operation-source](draft/formation-operation-source.md)
- Runtime Network (draft):
  [ADR-021](draft/ADR-021-runtime-network-phase1.md),
  [ADR-028](draft/ADR-028-runtime-network-search-unknown.md),
  [ADR-031](draft/ADR-031-runtime-network-search-budget.md),
  [ADR-032](draft/ADR-032-runtime-network-source-objects.md),
  [ADR-033](draft/ADR-033-formation-retained-replay.md),
  [ADR-034](draft/ADR-034-formation-search-state.md),
  [runner-worker-isolation](draft/runner-worker-isolation.md)
- Source freeze and resolver (draft):
  [ADR-023](draft/ADR-023-source-resolver-v2-contained-symlinks.md)
- Browser evidence and hosted browser computation (draft):
  [ADR-020](draft/ADR-020-formation-browser-contract-v0.md),
  [ADR-022](draft/ADR-022-browser-verifier-containment.md),
  [BROWSER_PROTOCOL_ADAPTER_EXTENSION_V0](draft/BROWSER_PROTOCOL_ADAPTER_EXTENSION_V0.md),
  [HOSTED_BROWSER_COMPUTATION_V1](draft/HOSTED_BROWSER_COMPUTATION_V1.md)
- Records, objects, and runtime authority (draft):
  [ASYNC_RECORD_WRITER_FRONTIER](draft/ASYNC_RECORD_WRITER_FRONTIER.md),
  [CAPSULE_OBJECT_TRANSPORT_CLIENT](draft/CAPSULE_OBJECT_TRANSPORT_CLIENT.md),
  [CONNECTED_REALIZATION_WORKER](draft/CONNECTED_REALIZATION_WORKER.md),
  [CONTRACT_VERIFIER_REGISTRY](draft/CONTRACT_VERIFIER_REGISTRY.md),
  [COMPUTATION_RESIDUAL_IDENTITY_REDESIGN](draft/COMPUTATION_RESIDUAL_IDENTITY_REDESIGN.md),
  [RECORD_OPERATION_MODEL_V2](draft/RECORD_OPERATION_MODEL_V2.md),
  [RUNTIME_EVOLUTION_AUTHORITY_V1](draft/RUNTIME_EVOLUTION_AUTHORITY_V1.md),
  [RUNTIME_OBJECT_GRAPH_AGENTS](draft/RUNTIME_OBJECT_GRAPH_AGENTS.md),
  [VM_SNAPSHOT_MATERIALIZATION](draft/VM_SNAPSHOT_MATERIALIZATION.md)
- App surfaces (draft):
  [ACTIVITY_OPERATION_ADAPTER_V0](draft/ACTIVITY_OPERATION_ADAPTER_V0.md),
  [TAURI_DESKTOP_MIGRATION](draft/TAURI_DESKTOP_MIGRATION.md)

Files not listed above are likewise non-normative drafts; check each file and
the merged implementation before relying on it.
