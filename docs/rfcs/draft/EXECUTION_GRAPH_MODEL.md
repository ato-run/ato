# RFC: Ato Execution Graph Model

**Status**: Draft  
**Target milestone**: v0.6.x / v0.7 foundation  
**Issue**: ato-run/ato#490 (implementation umbrella)  
**Related**: #74 (v0.6 graph migration), execution receipts, execution identity, runtime observation, drift detection

---

## Summary

This RFC defines the Ato Execution Graph Model.

Ato represents a launch as a graph. The graph is the source of truth for
execution identity, launch replay, teardown, receipts, drift detection, and
reproducibility classification.

The model has three graph layers:

- **`G_declared`** — what the capsule, inferred manifest, and launch profile declare.
- **`G_resolved`** — what Ato resolves and prepares before launch.
- **`G_observed`** — what Ato observes during execution.

Each graph layer has a canonical form and a content-addressed identity:

```
declared_execution_id = H(canonical(G_declared))
resolved_execution_id = H(canonical(G_resolved))
observed_execution_id = H(canonical(G_observed))
```

The purpose is **not** to guarantee bit-identical process behavior across
platforms. The purpose is to make launch conditions explicit, comparable,
replayable, and diagnosable. Ato records declared/resolved execution evidence
and derives stable launch-envelope identities from canonical graph content.

Reproducibility is classified as a **class**, not a boolean: `pure`,
`host-bound`, `state-bound`, `time-bound`, `network-bound`, `best-effort`.

A receipt is therefore not a marketing claim that "this execution is
reproducible." It is a structured record of what Ato declared, resolved,
observed, and could not observe.

## Implementation status (as of this RFC)

This RFC documents the target model. The current codebase implements the
declared/resolved layers; the observed layer and several evidence fields are
still placeholders.

**Implemented (load-bearing in production):**

- Graph construction — `ExecutionGraphBuilder` → `LaunchGraphBundle`
  (`crates/capsule-core/src/engine/execution_graph/`).
- Canonicalization and the declared/resolved identity split —
  `declared_execution_id` / `resolved_execution_id` from
  `canonical(G_declared)` / `canonical(G_resolved)`
  (`canonical.rs`, `CANONICAL_FORM_VERSION = 1`).
- `SessionRecord.graph` (`StoredExecutionGraph`) is populated for production
  launches.
- Graph-driven teardown reverse-traverses the stored graph.
- Execution receipts persist `declared_execution_id` / `resolved_execution_id`.

**Placeholder / not implemented yet** (tracked under #490):

- Non-empty `NodeReceipt` / `EdgeReceipt` evidence — currently emitted empty.
- Computed `GraphCompleteness` with reasons — currently always `Partial`.
- `ObservationScope` in receipts.
- `ReproducibilityClass` derived from graph facts (conservative classification).
- Runtime observation (`G_observed`) and `observed_execution_id` — `None` today.
- Drift detection.

**Discipline while observation is unimplemented:** observation is not
implemented yet. Until observation lands, receipts must not synthesize
`observed_execution_id`, and `GraphCompleteness` must not overclaim completeness
(`Complete` is not emitted; completeness stays `Partial` with explicit reasons).

## Motivation

Ato runs source-native projects: local directories, GitHub repositories, shared
app handles, and installed app profiles. For these workloads, artifact identity
alone is insufficient. A source tree can build reproducibly yet execute
differently because of: runtime binary selection, package manager version,
dependency materialization, native ABI, environment closure, filesystem view,
mounts and state bindings, network policy, capability policy, entrypoint,
arguments, working directory, timezone, host tools, GPU/driver/kernel behavior.

Ato already has parts of this system (ExecutionGraph, declared/resolved IDs,
`SessionRecord.graph`, graph-based teardown, execution receipts,
reproducibility classes, `GraphCompleteness`, `NodeReceipt`/`EdgeReceipt`
placeholders). However, the model is not yet specified tightly enough:
`observed_execution_id` is not populated, `GraphCompleteness` is effectively
`Partial`, `NodeReceipt`/`EdgeReceipt` are reserved but mostly empty, drift
detection is not defined, and host-bound behavior is not consistently surfaced.

This RFC turns the graph work into a clear model with Nix-like invariants: all
relevant inputs are explicit, immutable outputs are separated from mutable
state, closures are computable, undeclared host dependencies are never
invisible, and identity is derived from canonical data rather than incidental
runtime state.

## Goals

1. Define the normative graph layers: declared, resolved, observed.
2. Define the identity semantics for each graph layer.
3. Define the node and edge taxonomy.
4. Define canonicalization requirements.
5. Define receipt semantics.
6. Define reproducibility classes and their derivation.
7. Define drift detection semantics.
8. Define how strict mode should treat undeclared host dependencies.
9. Provide an implementation roadmap that can be shipped incrementally.

## Non-goals

- Does not require deterministic record/replay.
- Does not require bit-identical process traces across all platforms.
- Does not turn Ato into Nix, Docker, or a system package manager.
- Does not require all observation hooks to be implemented before v0.6.0.
- Does not require blocking all host-bound executions by default.
- Does not require secrets to be hashed or stored. **Secret values must never
  enter execution identity, receipts, logs, or repair prompts.**

## Design principles

1. **ExecutionGraph is the source of truth.** Execution state should not be
   reconstructed from scattered runtime structs. The launch graph drives
   preflight, dependency contracts, consent, runtime preparation, launch,
   teardown, receipts, replay, and drift detection. Derived views are allowed,
   but they must be derived from the graph, not maintained as independent
   sources of truth.
2. **Declare, resolve, observe.** Ato must distinguish three questions: what was
   requested? (`G_declared`) what was prepared? (`G_resolved`) what actually
   happened? (`G_observed`). The layers must not be collapsed.
3. **Immutable outputs and mutable state are separate.** Source tree snapshots,
   runtime tool artifacts, dependency outputs, build artifacts, OCI image
   digests, and materialized web/static outputs are immutable/content-addressed
   where possible. Launch profiles, ports, consent records, secret references,
   persistent state, session-local tmp, logs, receipts, and repair proposals
   are mutable instance/session context. Execution identity may reference
   mutable state bindings but must not pretend mutable state is immutable.
4. **Reproducibility is classified, not asserted.** Ato emits a class and
   supporting evidence, not a single `reproducible = true` boolean.
5. **Unknown host dependency is never invisible.** If a launch uses host state,
   tools, paths, services, or capabilities outside the declared/resolved graph,
   the result must be classified. Permissive profiles may warn; strict profiles
   may error.
6. **Receipts are evidence envelopes.** A receipt distinguishes declared,
   resolved, observed, inferred facts, unobserved gaps, and redacted values.
   Receipts must never contain secret values.

## Graph layers

### G_declared

Constructed from static, user-declared input: `capsule.toml`, inferred
manifest, `ato.lock.json` declarations, launch profile, target label, service
graph declarations, network/capability/state-binding declarations, environment
allowlist, entrypoint/argv/cwd declarations. Answers: *what does this
capsule/profile say it needs?*

### G_resolved

Constructed after Ato resolves concrete objects: resolved source tree hash,
runtime binary identity, runtime tool identity, dependency derivation hash,
dependency output hash, build artifact hash, filesystem view, network policy
hash, capability policy hash, state binding identity, service graph, env
closure, entrypoint/argv/cwd. Answers: *what did Ato prepare before launch?*

### G_observed

Constructed from runtime observations: spawned process tree, executed host
tools, filesystem access events, network egress attempts, environment reads,
dynamic library loads, state reads/writes, service readiness, sandbox denials,
capability bridge calls. Answers: *what did the process actually use?*
`G_observed` may be incomplete; incomplete observation must be represented
explicitly through `GraphCompleteness`. **Not implemented yet.**

## Identity semantics

`declared_execution_id = H(canonical(G_declared))` — changes when the requested
launch envelope changes (manifest target, declared runtime constraint, declared
network/capability policy, declared state binding, entrypoint/argv/cwd, launch
profile declarations). Must **not** change because of dynamic port, pid,
container id, wall-clock timestamp, session-local temp path, log path, or
receipt path.

`resolved_execution_id = H(canonical(G_resolved))` — changes when the concrete
resolved launch envelope changes (source tree hash, runtime/tool binary hash,
dependency output hash, filesystem view, network/capability policy hash, state
binding identity, entrypoint/argv/cwd). Must **not** change because of pid,
container id, readiness timestamp, log file path, non-semantic JSON field
ordering, or session-local random temp suffix not visible to the process.

`observed_execution_id = H(canonical(G_observed))` — changes when observed
execution behavior changes. May be absent when observation is unavailable. Must
be paired with `GraphCompleteness`, `ObservationScope`, and the observation
backend. An observed identity is a fingerprint of observed runtime facts, not a
proof of full determinism. **Not implemented yet; not synthesized today.**

## Canonicalization

All graph identities must be derived from a deterministic canonical form. The
canonical form must: include an explicit schema version; include the graph
layer name; sort nodes, edges, and map keys deterministically; normalize paths,
platform triples, and policy representation where appropriate; exclude
non-semantic session-local identifiers; and preserve semantically meaningful
values exactly.

The canonical form must **not** depend on `HashMap` iteration order, JSON
serializer incidental ordering, absolute host cache paths (unless exposed to
the process), log/receipt paths, process ids, container ids, or timestamps
(unless time is part of the launch envelope).

Recommended framing:

```
AtoCanonicalGraphV1 {
  schema_version: 1,
  layer: declared | resolved | observed,
  nodes: [...sorted...],
  edges: [...sorted...],
  metadata: {...sorted...},
}
```

## Node model

The initial node taxonomy is intentionally small and stable:

`SourceNode`, `RuntimeNode`, `RuntimeToolNode`, `DependencyDerivationNode`,
`DependencyOutputNode`, `BuildDerivationNode`, `BuildArtifactNode`,
`FilesystemViewNode`, `EnvNode`, `NetworkPolicyNode`, `CapabilityPolicyNode`,
`StateBindingNode`, `EntrypointNode`, `ServiceNode`, `ProcessNode` (observed),
`HostObservationNode` (observed).

Representative fields:

- **SourceNode** — `source_ref` (provenance), `source_tree_hash` (materialized
  identity), `materialization_policy`, `source_origin`.
- **RuntimeNode** — `runtime_kind` (python/node/deno/native/wasmtime/
  browser_static/oci), `version_constraint`, `resolved_version`, `platform`,
  `binary_hash`, `provider`.
- **RuntimeToolNode** — auxiliary tools (uv/pnpm/yarn/bun): `tool_name`,
  `version_constraint`, `resolved_version`, `platform`, `archive_hash`,
  `binary_hash`, `provider`. *Deno is runtime-modeled, not a RuntimeToolNode.*
- **DependencyDerivationNode** — `ecosystem`, `input_files`, `package_manager`,
  `package_manager_version`, `install_command`, `lifecycle_script_policy`,
  `network_policy`, `platform`, `environment_closure_hash`.
- **DependencyOutputNode** — `output_hash`, `output_layout`, `platform`,
  `native_artifact_boundary`.
- **BuildDerivationNode** / **BuildArtifactNode** — how a build artifact is
  produced / the built output. `artifact_build_id` is **not** `execution_id`.
- **FilesystemViewNode** — `mounts` (each: `source_identity`, `destination`,
  `mode`, `durability`, `visibility`), `case_sensitivity`, `symlink_policy`,
  `tmp_policy`, `cwd`.
- **EnvNode** — `allowed_env_names`, `fixed_env_values_or_hashes`,
  `redacted_env_refs`, `excluded_env_names`, `baseline_env_policy`. **Secret
  values must not be included.**
- **NetworkPolicyNode** — `mode`, `allowed_hosts`, `allowed_cidrs`,
  `sidecar_policy`, `dns_policy`, `enforcement_status`.
- **CapabilityPolicyNode** — `fs_read`, `fs_write`, `host_bridge`,
  `open_external`, `terminal`, `automation`, `unsupported_capabilities`,
  `enforcement_status`.
- **StateBindingNode** — `state_id`, `kind`, `durability`, `purpose`,
  `attach_mode`, `compatibility_contract`, `snapshot_ref`.
- **EntrypointNode** — `entrypoint`, `argv`, `cwd`, `shell_policy`.
- **ServiceNode** — `service_name`, `target`, `depends_on`, `readiness_probe`,
  `restart_policy`, `run_once`.
- **ProcessNode** / **HostObservationNode** — observed-only nodes (process
  identity with redacted/session-local pid; host dependency observations with
  redacted path/resource and classification).

## Edge model

Initial edge taxonomy: `declares`, `resolves_to`, `depends_on`, `materializes`,
`mounts`, `allows`, `requires`, `launches`, `observed_access`, `observed_write`,
`observed_network`, `observed_host_tool`, `observed_denial`.

Edges must be directional and typed. Examples:

```
SourceNode              --declares-->        RuntimeNode
RuntimeNode             --requires-->        RuntimeToolNode
DependencyDerivationNode --materializes-->   DependencyOutputNode
DependencyOutputNode    --mounts-->          FilesystemViewNode
NetworkPolicyNode       --allows-->          observed_network api.openai.com
ProcessNode             --observed_host_tool-> /usr/bin/git
ServiceNode             --depends_on-->       ServiceNode
```

## Reproducibility classes

Ato derives a reproducibility class from declared, resolved, and (when
available) observed graph facts.

- **pure** — expected to replay from the resolved execution identity alone:
  sealed source tree, sealed dependency output, sealed runtime binary, closed
  environment, read-only source/dependency view, no persistent state, no
  external network, no undeclared host tools, fixed entrypoint/argv/cwd,
  observation complete or sufficient.
- **host-bound** — depends on host-specific facts (host binary, host dynamic
  library, kernel feature, driver, GPU runtime, CPU feature, OS keychain,
  system service).
- **state-bound** — depends on persistent or previous state (database, user
  workspace, browser profile, model cache, previous generated files). May still
  be replayable if a compatible state snapshot/binding is available.
- **time-bound** — depends on time (wall-clock, timezone, scheduled behavior,
  monotonic time, date-sensitive logic).
- **network-bound** — depends on external network responses (API response,
  registry fetch, remote model endpoint, telemetry, OAuth/device flow).
- **best-effort** — observation or enforcement is incomplete (unsupported
  sandbox backend, unknown dynamic library loads, unknown host filesystem
  access, unobserved process subtree, unclassified native side effects).

**Until observation lands, this classification is conservative and
pre-observation:** it is derived from declared/resolved facts. For example
`network-bound` means *egress is allowed in the launch envelope*, not that the
process actually accessed the network.

### Class derivation rules

Classification should be monotonic in risk. Suggested precedence:
`best-effort > network-bound > time-bound > state-bound > host-bound > pure`.
However, receipts should preserve **all** contributing causes, not only the
final class:

```json
{
  "reproducibility_class": "network-bound",
  "class_reasons": [
    "persistent_state_binding:data",
    "network_egress:api.openai.com"
  ]
}
```

## Graph completeness

`GraphCompleteness` describes how much of the graph is known: `Complete`,
`Partial`, `Unavailable`.

- **Complete** — only if Ato can account for all configured observation domains
  for the selected runtime/profile. This does not mean deterministic behavior;
  it means Ato believes the graph is complete according to the declared
  observation scope. **`Complete` must not be emitted while runtime observation
  is unimplemented.**
- **Partial** — Ato knows some facts but observation is incomplete (e.g.
  source/runtime/dependency known but dynamic fs access unobserved; network
  policy known but actual egress unobserved). A `Partial` receipt must always
  carry non-empty `graph_completeness_reasons`.
- **Unavailable** — observation is not available for the runtime/backend/profile.

## Observation scope

Receipts must record the observation scope:

```
ObservationScope {
  filesystem:   none | mounts-only | coarse | detailed
  network:      none | policy-only | attempted-egress | full-proxy-log
  process:      none | root-only | process-tree
  env:          none | launch-closure-only | observed-reads
  host_tools:   none | command-spawn | path-resolution
  capabilities: none | bridge-calls
}
```

Observation scope determines whether `GraphCompleteness::Complete` is possible.

## Receipts

Execution receipts should include: `receipt_schema_version`, `session_id`,
`install_profile_key` (installed app), `install_revision_id` (installed app),
`capsule_instance_key` (if exact replay key exists), `declared_execution_id`,
`resolved_execution_id`, `observed_execution_id` (if available),
`graph_completeness`, `observation_scope`, `reproducibility_class`,
`class_reasons`, `node_receipts`, `edge_receipts`, `policy_enforcement`,
`redactions`, `failure_envelope` (if failed).

- **NodeReceipt** — `node_id`, `node_kind`, `layer`, `identity`,
  `evidence_kind`, `evidence_ref`, `completeness`, `redaction_status`.
- **EdgeReceipt** — `edge_id`, `edge_kind`, `from_node`, `to_node`, `layer`,
  `evidence_kind`, `evidence_ref`, `completeness`.

## Drift detection

Drift is a difference between execution identities or graph components for
apparently similar user intent. Basic definition: same `source_ref` + different
`execution_id` = drift. At least three drift classes:

- **DeclaredDrift** — the requested launch changed (manifest, launch profile,
  network policy, entrypoint, state binding declaration).
- **ResolvedDrift** — Ato resolved different concrete objects for the same
  declaration (same runtime constraint / different binary hash; same lockfile /
  different package-manager binary; same source ref / different materialized
  tree; same dependency declaration / different output hash; same service
  declaration / different resolved service graph).
- **ObservedDrift** — runtime behavior differed (new network egress, new host
  tool usage, new state write, new dynamic library load, new denied access,
  different process tree).

Drift output should be **component-level**, not just ID-level:

```
resolved_execution_id changed because:
  RuntimeToolNode uv binary_hash changed
  DependencyOutputNode node_modules hash changed
```

## Strict profile behavior

A strict profile should gradually move from warnings to fail-closed behavior.

Initial policy: undeclared host tool observed → warning + host-bound;
undeclared network egress observed → deny if policy enforced, otherwise warning
+ network-bound; undeclared state write → warning + state-bound; unsupported
observation backend → best-effort.

Future strict policy: undeclared host tool → hard error; undeclared network
egress → hard error; undeclared host path read/write → hard error; unsupported
observation for a strict runtime → hard error or explicit downgrade prompt.

## Relationship to the Ato install model

Installed app identity must remain distinct from execution identity:

- `install_profile_key` — stable user-facing key.
- `install_revision_id` — immutable installed artifact/output revision.
- `execution_id` — launch envelope identity.
- `capsule_instance_key` — exact replay/session key, recommended:
  `H(install_profile_key, install_revision_id, resolved_execution_id)`.

OS shortcuts and Dashboard entries must point to `install_profile_key`, not to
`execution_id` or `capsule_instance_key`.

## Relationship to artifact build identity

`artifact_build_id` is **not** `execution_id`. It identifies reusable build
outputs and should include source tree identity, build derivation identity,
dependency output identity, target platform, OS/arch/ABI boundary, and native
addon compatibility boundary. It should **not** include session id, process id,
dynamic port, container id, log path, receipt path, runtime pid, or
launch-only mutable state. `execution_id` identifies a launch envelope.

## Implementation roadmap

This RFC ships incrementally. The near-term milestone (#490) makes the
declared/resolved evidence path real before any observation.

- **P0 (#491)** — land this RFC; fix stale graph-migration comments.
- **P1 (#492)** — canonical graph contract tests (deterministic IDs; session
  metadata absent from graph input; semantic mutations change IDs).
- **P2 (#493)** — non-empty `NodeReceipt`/`EdgeReceipt` from existing
  declared/resolved graph data.
- **P4 (#495)** — `ObservationScope` metadata in receipts (no fake
  `observed_execution_id`).
- **P3 (#494)** — derive `GraphCompleteness` (Partial + reasons; no `Complete`
  yet) and conservative pre-observation `ReproducibilityClass`.
- **P5 (#496)** — component-level drift diff v1.

Later (filed when the evidence layer lands): minimal runtime observation
(`G_observed`), `observed_execution_id`, strict-profile enforcement, Desktop
graph/drift viewer.

## Acceptance criteria

The implementation is minimally complete when: declared/resolved execution IDs
are stable and tested; `SessionRecord.graph` is populated for production
launches; graph-based teardown uses the stored graph where available; execution
receipts include non-empty node/edge receipts for declared/resolved graph data;
`GraphCompleteness` reflects actual observation coverage (with reasons); and
stale graph-migration comments are removed.

The implementation is fully complete when: `G_observed` is populated for
supported runtimes; `observed_execution_id` is computed where observation is
available; node/edge receipts include observed facts; drift detection can
compare receipts; strict profile can fail on selected undeclared host
dependencies; and Desktop can explain graph drift to users.

## Open questions

1. Should `observed_execution_id` be absent when observation is `Partial`, or
   present with `GraphCompleteness::Partial`?
2. Which observation backends are required for `GraphCompleteness::Complete`
   per runtime?
3. Should host absolute paths be redacted, hashed, or stored as-is in
   local-only receipts?
4. Should network DNS resolution be part of `NetworkPolicyNode`, `G_observed`,
   or both?
5. How should time/timezone be represented in `EnvNode` vs a dedicated
   `TimeNode`?
6. Should state snapshots have first-class content hashes or remain external
   references?
7. How much graph data should live in `SessionRecord` vs only in execution
   receipts?
8. Should graph canonicalization use a custom canonical form or RFC 8785 JSON
   canonicalization? (Current implementation uses a custom deterministic form,
   `CANONICAL_FORM_VERSION = 1`.)
9. What is the minimum observation set required for v0.6.x?
10. Which strict-mode violations should become hard errors first?
