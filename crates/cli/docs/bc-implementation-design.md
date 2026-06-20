# B/C Implementation Design

This document captures the implementation design for the two remaining topics before code changes begin:

- B: fail-closed cleanup, explicit error classification, and manifest remediation hints
- C: multi-service DAG orchestration with phase synchronization

It is intentionally more concrete than the product spec and ties decisions to the current repository structure.

## 1. Scope

This design assumes the current hourglass model is preserved.

- Install keeps a single meaning: a clean artifact installation into a concrete directory root
- Consumer flow remains Install -> Prepare -> Build -> Verify -> DryRun -> Execute
- Producer flow remains Prepare -> Build -> Verify -> Install -> DryRun -> Publish
- Services mode continues to run inside the existing runtime/supervisor surface rather than introducing a second orchestration stack

This design does not change the public command surface yet. It defines the internal contracts needed to implement the spec already added to current-spec.

## 2. Goals

### 2.1 B Goals

- Every pipeline attempt either commits a complete phase result or leaves no partial state behind
- The primary error is always preserved and assigned a stable code and classification
- Manifest problems are separated from source/runtime problems
- When the root cause is declarative, ato can attach a concrete remediation hint and optional machine-readable fix proposal
- Cleanup results are observable in JSON mode without drowning out the root cause

### 2.2 C Goals

- Services mode remains deterministic under partial failure
- No service enters Execute until the whole graph has crossed the pre-execute barrier
- Runtime startup order remains DAG-based and readiness-gated
- A single service failure triggers graph-wide fail-fast shutdown and cleanup
- The design reuses the existing pipeline vocabulary instead of inventing a second lifecycle model for services

## 3. Non-Goals

- No attempt to auto-fix source code errors
- No transactional rollback of externally visible registry publishes beyond the current fail-closed boundaries
- No support for mixed-phase graph execution where some services are still building while others are already serving traffic
- No public exposure of every internal phase as a CLI flag

## 4. B Design: Cleanup, Error Classification, and Remediation

### 4.1 Core Decision

Cleanup is modeled as a per-attempt journal owned by the application pipeline layer, not by ad hoc Drop logic scattered across adapters.

The journal records compensating actions for resources created during the current attempt. Phases may register actions as soon as a side effect becomes possible. When a phase result is committed, the corresponding cleanup actions are either dropped or transformed into longer-lived ownership handled elsewhere.

This keeps the failure contract explicit and phase-aware.

### 4.2 New Internal Concepts

#### PipelineAttemptContext

A per-invocation context passed through phase execution.

Responsibilities:

- owns a CleanupJournal
- owns attempt-scoped diagnostics
- records the current phase boundary
- records whether the attempt reached a committed terminal state

Likely home:

- src/application/pipeline/executor.rs
- or a new src/application/pipeline/attempt.rs if executor starts growing too much

#### CleanupJournal

An ordered stack of cleanup actions registered by phases and adapters.

Properties:

- append-only during forward execution
- unwind in reverse order on failure
- each action returns a structured cleanup result rather than only Result<()>
- cleanup failures are collected, not allowed to replace the root failure

Likely action families:

- remove_temp_dir
- remove_extract_root
- stop_sidecar
- kill_child_process
- delete_generated_manifest_if_uncommitted
- revert_projection
- release_reserved_port

Likely home:

- new src/application/pipeline/cleanup.rs

#### CleanupScope

A small helper object created inside a phase when that phase starts staging resources.

Responsibilities:

- register actions against the attempt journal
- mark actions committed when ownership transfers out of the phase
- make phase code read like staged work instead of raw cleanup bookkeeping

This gives the code a ScopeGuard-like shape without hiding cleanup policy in Drop side effects alone.

#### ClassifiedPipelineError

An internal error view layered on top of existing AtoExecutionError and anyhow propagation.

Fields:

- code
- name
- phase
- classification
- retryable
- hint
- manifest_suggestion
- causes
- cleanup_report

Likely classification values:

- manifest
- source
- provisioning
- execution
- internal

Likely home:

- extend core/src/execution_plan/error.rs and the JSON mapping path in src/utils/error.rs
- keep final rendering in src/adapters/output/diagnostics

#### ManifestSuggestion

Machine-readable remediation proposal for declarative failures.

Initial shape:

```json
{
  "kind": "set_field",
  "path": "targets.app.required_env",
  "operation": "append",
  "value": "DB_PASS",
  "message": "Add DB_PASS to required_env"
}
```

Initial kinds:

- set_field
- append_list_item
- replace_enum_value
- create_table
- remove_conflicting_field

This stays advisory at first. Interactive application can come later.

### 4.3 Control Flow

#### Forward path

1. CLI builds the request as it does today
2. Pipeline executor creates a PipelineAttemptContext
3. Each phase receives access to the attempt context
4. As soon as a phase creates a temp root, launches a helper, or writes an uncommitted artifact, it registers the matching cleanup action
5. If the phase completes and ownership transfers permanently, it commits or clears those staged cleanup actions
6. If the phase returns an error, executor stops forward progress and moves into cleanup

#### Failure path

1. Executor captures the root error and current phase
2. CleanupJournal unwinds in reverse order
3. Cleanup results are aggregated into a CleanupReport
4. Root error is classified and enriched
5. JSON and human diagnostics are rendered from the enriched result

### 4.4 Classification Rules

#### Manifest

Use when the failure is caused by declarative contract mismatch.

Examples:

- missing required_env declaration
- invalid services graph declaration
- unsupported runtime/driver combination in manifest
- missing field required by the selected target model

Primary producers:

- existing manifest validation paths
- inference validation
- IPC and services validation logic already under src/adapters/ipc/validate.rs

#### Source

Use when the manifest is valid but the user code or assets fail.

Examples:

- compiler error
- bad entrypoint script
- import or module not found inside source tree
- process exits because application code panics

#### Provisioning

Use for fetch, install, unpack, environment preparation, and launch staging failures.

Examples:

- artifact download failed
- archive extraction failed
- sandbox root could not be created
- required runtime tool could not be provisioned

#### Execution

Use after the attempt has crossed into Execute.

Examples:

- readiness probe timeout
- child process crash
- fail-fast supervisor shutdown after one service exits

### 4.5 Remediation Production

Manifest remediation is produced only when all of the following are true:

- the root error is manifest-classified
- the missing or invalid declaration can be mapped to a specific manifest path
- ato can propose an edit without guessing user intent too broadly

Initial remediation sources:

- missing required environment declaration inferred from runtime preflight
- missing services.main in services mode
- invalid depends_on cycle with explicit cycle path
- unsupported target field combinations that have one canonical replacement

Initial non-remediated cases:

- ambiguous inference conflicts
- source build failures
- external service outages

### 4.6 Module Changes

#### application/pipeline

- executor owns PipelineAttemptContext lifecycle
- new cleanup module owns CleanupJournal and CleanupReport
- phase helpers accept mutable attempt context or a narrower cleanup handle

#### common/sidecar

- sidecar handles remain responsible for low-level stop logic
- attempt cleanup wraps them as a registered compensating action instead of relying on scattered ad hoc caller cleanup

#### adapters/output/diagnostics

- mapping layer expands to include classification, cleanup status, and manifest_suggestion
- human output can become richer later, but JSON shape should be stabilized first

#### utils/error

- JSON emission path gains the new optional fields
- fallback anyhow heuristics should map into the classification model instead of bypassing it

### 4.7 Rollout Plan for B

1. Add CleanupJournal and CleanupReport with no behavior change beyond executor plumbing
2. Register cleanup for the highest-risk temporary resources in run and install paths
3. Extend JSON envelope fields and diagnostics mapping
4. Introduce manifest_suggestion for a narrow set of deterministic manifest failures
5. Expand coverage to publish, install, and services execution paths

## 5. C Design: Services Graph Barrier and Supervisor

### 5.1 Core Decision

Services orchestration is split into two layers:

- pre-execute graph preparation uses phase barriers
- execute uses DAG order plus readiness gating under a single supervisor

This preserves the hourglass model while keeping service startup deterministic.

### 5.2 New Internal Concepts

#### ServiceGraphPlan

A validated graph representation built from top-level services metadata.

Contains:

- nodes keyed by service name
- edges from depends_on
- topological layers for Execute startup
- optional per-service phase state

Likely home:

- new src/application/services/graph.rs
- validation can reuse ideas from src/adapters/ipc/validate.rs and core service types

#### ServicePhaseCoordinator

Coordinates Install, Prepare, Build, Verify, and DryRun for all services before any service is admitted to Execute.

Responsibilities:

- schedule per-service work for the current phase
- wait for all services in that phase to finish
- short-circuit on first failure
- emit graph-wide abort on failure

Likely home:

- new src/application/services/coordinator.rs

#### ServiceExecutionSupervisor

Owns Execute once the graph barrier is crossed.

Responsibilities:

- start root services first
- gate dependent services on readiness of dependencies
- monitor child processes
- trigger fail-fast shutdown if any service exits unexpectedly or readiness fails

Likely integration point:

- adapt or wrap the current runtime services executor in src/adapters/runtime/executors/web_services.rs
- keep low-level spawning there, but move graph policy into application-facing orchestration

#### ServiceGraphFailure

Structured failure for services mode.

Contains:

- service_name
- phase
- classification
- root error code
- sibling_shutdown_results
- cleanup_report

### 5.3 Control Flow

#### Graph setup

1. Parse services from manifest data
2. Validate that services.main exists where required
3. Validate DAG and reject cycles before any side effects
4. Materialize ServiceGraphPlan

#### Pre-execute barrier

For each phase in Install, Prepare, Build, Verify, DryRun:

1. Run the phase for each service using service-local inputs and targets
2. Collect results for the whole graph
3. If all succeed, advance the barrier
4. If any fails, abort the graph, unwind cleanup journal, and return one graph failure result

The coordinator may run service work concurrently, but the barrier means the graph state only advances one phase at a time.

#### Execute

1. Start services in topological order
2. For each service, wait until its declared predecessors satisfy readiness_probe
3. Admit dependent services once their dependencies are ready
4. Continue until the whole graph is live
5. If any service exits or readiness fails, supervisor broadcasts shutdown to all running services

### 5.4 Why Barrier Before Execute

Without a barrier, the system can enter a mixed-phase state where one service is already exposed while another is still building or validating. That makes rollback and failure attribution much harder.

The barrier gives three properties we want:

- the graph is either globally ready for execution or not
- a failure before Execute has no partially live service graph to reconcile
- diagnostics can refer to one service as the root failure without pretending the graph was healthy

### 5.5 Concurrency Model

The design allows concurrency inside a phase, but not across phases.

- Install, Prepare, Build, Verify, and DryRun may process independent services concurrently
- phase completion is committed only when all services in that phase complete successfully
- Execute is partially concurrent by DAG layer and readiness

This gives useful parallelism without losing determinism.

### 5.6 Integration with B

Services mode uses the same CleanupJournal and classification pipeline.

- each service phase registers cleanup into the graph attempt context
- graph abort unwinds all staged resources for all services in reverse order
- readiness timeout becomes execution classification, not manifest classification
- DAG cycle rejection is manifest classification and can carry a manifest_suggestion later

### 5.7 Module Changes

#### New application layer

Add an application-level services orchestration module, for example:

- src/application/services/mod.rs
- src/application/services/graph.rs
- src/application/services/coordinator.rs
- src/application/services/supervisor.rs

Rationale:

- graph policy belongs to application semantics, not only to runtime adapters
- low-level process spawning still belongs in adapters/runtime

#### adapters/runtime/executors/web_services.rs

Refactor toward a thinner runtime executor.

- keep process construction, readiness probing primitives, and prefixed log streaming here
- move graph ordering and fail-fast policy decisions upward into application/services

#### core types

- reuse existing service types where possible
- only add graph-specific execution state outside core unless it becomes manifest-facing

### 5.8 Rollout Plan for C

1. Extract service graph validation into a reusable graph plan builder
2. Introduce a coordinator that can run no-op phase barriers first
3. Wire real Install, Prepare, Build, Verify, and DryRun service-local execution under the barrier
4. Refactor web_services executor into a supervisor backend used by the coordinator
5. Add structured graph failure reporting and fail-fast shutdown summaries

## 6. Testing Strategy

### 6.1 B Tests

- unit tests for CleanupJournal unwind order and partial cleanup failure aggregation
- phase tests that verify temp directories and sidecars are removed on failure
- JSON envelope snapshot tests for manifest, source, provisioning, and execution classifications
- remediation tests for deterministic manifest_suggestion generation

### 6.2 C Tests

- graph validation tests for cycles and missing dependencies
- barrier tests proving no service enters Execute early
- readiness timeout tests producing graph-wide fail-fast shutdown
- multi-service integration tests for one-service crash causing full shutdown

## 7. Open Questions

- Whether manifest_suggestion should stay purely advisory at first or also carry enough information for an editor-assisted patch flow
- Whether cleanup_actions in JSON should expose only action names or also status per action from day one
- Whether services mode should allow configurable shutdown ordering on fail-fast or always use reverse startup order

## 8. Recommended Implementation Order

1. B foundation: cleanup journal, enriched error envelope, classification mapping
2. B remediation: narrow deterministic manifest suggestions
3. C graph planning: DAG validation and barrier skeleton
4. C supervisor split: move graph policy to application/services and leave spawning in runtime adapter
5. Cross-cutting tests and snapshots
