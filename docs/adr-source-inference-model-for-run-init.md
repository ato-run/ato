# ADR: Source Inference Model For ato run / ato init

- Status: Proposed
- Date: 2026-03-25
- Decision Makers: ato-cli maintainers
- Related: [docs/current-spec.md](docs/current-spec.md), [docs/adr-ato-lock-json-canonical-input.md](docs/adr-ato-lock-json-canonical-input.md), [docs/bc-implementation-design.md](docs/bc-implementation-design.md)

## 1. Context

`ato.lock.json` is the canonical input for execution under the lock-first architecture. However, many real projects and onboarding flows still begin from source-only inputs.

Typical examples include:

- a local directory without `ato.lock.json`
- a freshly cloned repository
- a GitHub shorthand such as `github.com/owner/repo`
- a source tree with ecosystem manifests and lockfiles but no Ato-managed lock

This creates a necessary transitional problem:

- `ato run` must be able to execute from source when no canonical lock exists yet
- `ato init` must be able to materialize a durable `ato.lock.json` from source
- the system must preserve lock-first semantics without falling back to ad hoc manifest-first execution

The core question is not whether inference is allowed. The core question is how much may be inferred deterministically, what must remain unresolved, and what may only be resolved through sandboxed execution, user confirmation, or host-local binding.

This ADR defines the source inference model for `ato run` and `ato init`.

## 2. Decision

ato-cli defines a shared source inference pipeline for `ato run` and `ato init`.

Both commands use the same inference and resolution engine, but they differ in materialization scope and durability semantics.

- `ato run` performs attempt-scoped materialization
- `ato init` performs workspace-scoped materialization

More precisely:

- `ato run` may start from source, but it must synthesize an ephemeral canonical lock state and host-local binding before execution
- `ato init` may start from source, but it must synthesize and persist a workspace `ato.lock.json` and materialize it into the target workspace directory together with optional workspace-local state

Canonical lock state in this ADR may be fully resolved or explicitly partially resolved. Unresolved state must be represented by first-class schema markers rather than by silent omission.

The canonical lock state synthesized during inference should conform to the same logical lock-shaped model as `ato.lock.json`, even when it is ephemeral and not persisted. Execution-only metadata may exist outside the canonical hashed projection, but downstream consumers must operate on the canonical lock-shaped model rather than directly on source heuristics.

Unresolved state should preserve reason classes such as insufficient evidence, ambiguity, deferred host-local binding, policy-gated resolution, explicit user selection required, or bounded observation required.

Inference must prioritize portable core data:

- `contract`
- `resolution`

Inference must not prematurely freeze environment-specific data:

- actual host port allocation
- host-local paths
- concrete secret values
- organization-local enforcement policy
- runtime attestations and approvals

These belong to `binding`, host-local state, or later approval flow.

This ADR does not change the bidirectional hourglass pipeline defined in [docs/current-spec.md](docs/current-spec.md). It defines how source inputs are converted into canonical lock state before downstream execution semantics continue.

## 2.1 Implementation Status (2026-03-27)

This ADR is still `Proposed`, but large parts of the core handoff are already implemented. The purpose of this section is to separate current implementation fact from still-target design.

Implemented:

- a shared source inference engine exists and handles `SourceEvidence`, `DraftLock`, and `CanonicalLock` through a common infer -> resolve -> materialize entrypoint
- canonical `ato.lock.json` input is reused as authoritative lock-shaped state rather than being semantically re-inferred
- `ato run` performs attempt-scoped materialization by generating an attempt-local `ato.lock.json` plus provenance sidecar before entering downstream execution flow
- `ato init` performs workspace-scoped materialization by writing durable `ato.lock.json` plus workspace-local `.ato/source-inference/provenance.json`, `.ato/source-inference/provenance-cache.json`, and `.ato/binding/seed.json`
- equal-rank process ambiguity is handled fail-closed: `run` requires selection or fails, while durable output may preserve unresolved markers
- execution-critical preconditions are enforced before `run` proceeds: `contract.process`, `resolution.runtime`, `resolution.resolved_targets`, and `resolution.closure` must be present
- compatibility inputs are already funneled through the shared source inference handoff instead of remaining fully separate ad hoc execution paths

Implemented but still partial:

- the infer / resolve / materialize split now exists as explicit phase-oriented code paths: infer builds draft lock-shaped state and candidates, resolve performs deterministic promotion such as closure normalization and native-delivery build-derive, and materialize is the only phase that prepares single-script temporary workspaces or writes durable sidecars
- compatibility import is now treated as an import-side draft handoff instead of source heuristic reinference, and canonical lock input skips source candidate generation during infer
- equal-ranked candidate handling is deterministic (`score desc -> label asc -> entrypoint asc`) and remains fail-closed via unresolved markers plus selection gate
- downstream run/validate preparation now consumes materialized lock-derived bridge manifests instead of carrying source-origin manifest semantics as execute authority
- downstream lock-first consumption has progressed, but migration is still in flight and some compatibility helpers and transitional bridge artifacts remain
- durable output validation already requires execution-critical fields or explicit unresolved markers, but the semantic depth of some resolved fields is intentionally shallow
- `resolution.closure` currently normalizes into an explicit `kind`/`status` envelope; `metadata_only` remains allowed as `status = incomplete`, which satisfies lock-shaped handoff but does not yet represent a fully specified execution closure
- `build_closure.build_environment` currently uses array-based categories (`toolchains`, `package_managers`, `sdks`, `helper_tools`) so framework-specific producers can report multiple inputs without collapsing them into one scalar slot
- desktop native-delivery mode separation now appears in canonical contract state as `contract.delivery.mode = source-draft | source-derivation | artifact-import`
- compatibility import emits `contract.delivery.mode = artifact-import` for existing `.app` bundles, while native-delivery draft locks emit `source-draft` and resolve promotion upgrades them to `source-derivation`
- `contract.delivery.artifact.canonical_build_input` is fixed to `false` for imported artifacts and native build outputs; `.app` / `.exe` / AppImage / `.dmg` are not canonical build inputs
- provenance sidecars, selection-gate state, and inspect handoff are implemented, but they are still internal workspace state rather than a stabilized public schema

Not yet implemented:

- approval-gated and script-capable resolution is still placeholder-only; approval-required paths fail closed and explicit approval mode is not implemented yet
- general sandbox-assisted closure completion and safe promotion of generated ecosystem lockfiles into durable workspace state is not implemented as a complete policy
- some transitional install/build helpers still own bootstrap and preparation concerns outside the source inference core, even though execute authority now enters downstream through materialized lock-derived bridge manifests
- `closure_digest` is computed from the normalized `resolution.closure` envelope only when the closure is digestable and `status = complete`; incomplete metadata-only closure state does not produce a digest
- compatibility import can now emit `imported_artifact_closure` for an existing `.app` bundle on disk, and native-delivery source-derivation can promote an incomplete closure into `build_closure`; broader imported-artifact coverage across `.exe` / AppImage and richer closure completion remain future work
- the ADR-level goal of a fully explicit Ato-native closure/store/materialization model remains future work
- remote-source acquisition semantics, workspace ownership rules, and complete onboarding UX remain outside the implemented source inference core

When updating this ADR, keep this split current. Do not describe a target behavior as if it were already true unless it is either implemented or explicitly marked as future work here.

## 3. Definitions

### 3.1 Infer

Infer means deriving a lock skeleton from explicit evidence and deterministic heuristics.

Typical outputs include:

- `contract.process`
- `contract.network`
- `contract.filesystem`
- `contract.env_contract`
- `resolution.runtime`
- `resolution.resolved_targets` skeletons

### 3.2 Resolve

Resolve means turning an inferred skeleton into a concretely executable lock state.

Typical outputs include:

- target-specific runtime and toolchain selection
- dependency closure completion
- closure digests
- generated ecosystem lockfiles when required and safe
- verification-ready `resolution` entries

### 3.3 Materialize

Materialize means writing or synthesizing concrete state for a specific execution or workspace.

Examples:

- persisting `ato.lock.json`
- producing workspace-local binding state
- generating `config.json`
- generating execution plan artifacts

### 3.4 Ephemeral lock state

Ephemeral lock state is a fully formed canonical lock model used for one execution attempt but not written to the repo by default.

### 3.5 Confirmation and approval

This ADR distinguishes candidate selection or confirmation from security approval or consent.

- selection or confirmation resolves semantic ambiguity among candidates
- approval or consent authorizes risky, policy-gated, or trust-boundary-crossing behavior

Approval must not be treated as ordinary inference evidence.

## 4. Command Semantics

### 4.1 ato run

`ato run` is an attempt-scoped materialization command.

Responsibilities:

- accept existing canonical lock or source inputs
- infer and resolve missing canonical lock state when needed
- generate the execution artifacts needed for a specific run attempt
- synthesize host-local binding for the current machine
- execute against an immutable execution input for that attempt

For source-started execution, `ato run` must convert source input into lock-derived immutable execution input before Execute. It must not continue consulting ad hoc source heuristics as part of active execution semantics once canonical lock state for that attempt has been synthesized.

Default persistence rule:

- `ato run` must not persist repo-tracked `ato.lock.json` by default when starting from source

`ato run` may tolerate unresolved non-security-critical metadata for a single execution attempt, but it must fail before execution if required process, runtime, dependency closure, or security-sensitive contract fields remain unresolved.

Any unresolved field that can change execution semantics, selected process, capability surface, resource access, target compatibility, or trust boundary must be resolved before execution.

For `ato run`, unresolved handling is categorized as follows.

Must resolve before execute:

- process entrypoint or equivalent executable command
- selected runtime and required toolchain
- target compatibility for the current host or selected target
- dependency closure materialization required for the attempt
- required supervisor capability
- security-sensitive identity, secrets handling, privileged writes, and enforced network capability

May remain unresolved for one execution attempt:

- descriptive metadata
- optional config projection hints
- non-required environment variables
- non-security filesystem classification hints

Must not be silently guessed:

- secret values
- identity provider selection
- privileged host path writes
- externally exposed network approvals
- equal-ranked execution candidates

It may persist temporary or workspace-local artifacts such as:

- ephemeral lock cache
- generated ecosystem lockfiles in sandboxed temp space
- binding cache
- diagnostics and provenance traces

Persistent write-back from `run` is opt-in only.

Even when explicit write-back is enabled, `ato run` must not become a general workspace initialization flow. Durable project bootstrap, durable unresolved-state emission, and baseline workspace directory generation remain responsibilities of `ato init`.

### 4.2 ato init

`ato init` is a workspace-scoped materialization command.

Responsibilities:

- analyze source input
- infer canonical lock structure
- resolve enough runtime and closure data to create a durable project baseline
- materialize the durable baseline into the target workspace directory selected by surrounding acquisition or bootstrap flow
- write workspace `ato.lock.json`
- initialize optional workspace-local Ato state

This ADR does not define how a target directory is chosen, created, or overwritten. Within this ADR, `ato init` is responsible only for materializing durable lock-first workspace state into the target workspace directory once that target has been established by command or acquisition semantics.

The durable baseline produced by `ato init` must include at least:

- selected runtime and toolchain data sufficient for deterministic re-validation
- resolved target selection, or an explicit unresolved marker
- executable process contract, or an explicit unresolved marker
- dependency and closure state sufficient for deterministic re-resolution, or an explicit unresolved reason

Partially resolved durable output is allowed, but only when unresolved state is first-class, inspectable, and compatible with deterministic re-validation under the canonical-input ADR.

`ato init` is stricter than `ato run`.

- ambiguity may require prompt or explicit unresolved output
- dangerous capabilities must fail or prompt
- fallback behavior must be surfaced clearly
- provenance should be inspectable after generation

`ato init` may persist unresolved fields only when they are explicitly represented and do not undermine deterministic re-validation of the resulting workspace state.

### 4.3 Shared engine, different persistence

Both commands must reuse the same inference rules for consistency.

The difference is not what they infer. The difference is:

- whether the result is ephemeral or durable
- whether unresolved ambiguity is tolerated for immediate execution
- whether attempt-scoped or workspace-scoped state is materialized as part of the command contract

## 5. Inference Principles

### 5.1 Deterministic first

Inference must prefer explicit evidence and deterministic rules over dynamic execution.

### 5.2 Portable before local

Inference must populate portable `contract` and `resolution` data before host-local `binding` data.

### 5.3 Explicit evidence over observation

Static evidence must outrank sandbox observation whenever both are available.

### 5.4 Observation is allowed but bounded

Sandbox-assisted completion is allowed for closure completion, smoke verification, and safe resolution. It must not become the primary source of application semantics.

Sandbox-assisted resolution must run under an explicitly bounded policy. Source mutation, network access, generated artifact promotion, and script execution behavior must be controlled by command semantics and follow-up ADRs rather than left implicit.

Additional constraints:

- metadata-only resolution and script-capable resolution must be distinguished explicitly
- sandbox-assisted resolution must not access arbitrary host state by default
- generated artifacts must not be promoted into durable workspace state without explicit command semantics and retained provenance
- lifecycle scripts, postinstall hooks, and arbitrary build steps must not run implicitly during durable lock generation unless explicitly allowed
- sandbox-derived observations are lower-precedence evidence than explicit manifests, lockfiles, and deterministic specification rules for durable semantics
- script-capable resolution requires an explicit mode gate; bounded defaults may exist for `run`, but durable generation for `init` must require explicit allowance

### 5.5 Fail closed for security-sensitive fields

Identity, secrets, privileged filesystem writes, dangerous capabilities, and enforced network access must not be silently inferred into durable contract state from weak evidence.

### 5.6 Preserve provenance

Every inferred or resolved field should be traceable to its source evidence or derivation rule for diagnostics and inspectability.

## 6. Source Resolution Precedence

When constructing canonical lock state from source, the following precedence combines evidence ordering with later decision gates:

1. existing `ato.lock.json`
2. explicit ecosystem lockfiles
3. explicit ecosystem manifests and config
4. deterministic convention heuristics
5. sandbox-assisted resolution
6. user selection or confirmation
7. security approval or consent
8. unresolved state

This means the system should:

- first read
- then infer
- then safely test
- then ask
- then leave unresolved rather than guess unsafely

Selection or confirmation resolves ambiguity among viable candidates. Approval or consent authorizes risky or policy-gated actions and is not ordinary semantic evidence.

Steps 1 through 5 are evidence or resolution tiers. Steps 6 and 7 are decision gates applied after evidence-driven inference has been exhausted or blocked.

If multiple candidates remain after deterministic precedence is applied, the implementation must not silently choose among equal-ranked candidates.

- if a strict total order exists after applying spec-defined precedence, the highest-ranked candidate may be selected automatically
- if equal-ranked candidates remain, `ato run` must require explicit selection, confirmation, or fail
- if equal-ranked candidates remain, `ato init` must require explicit selection or persist an explicit unresolved marker
- a single weak heuristic candidate may still be left unresolved by `ato init` rather than being durable by default

## 7. Common State Machine

The shared source inference state machine is:

1. Acquire source
2. Detect existing canonical lock
3. Infer lock skeleton
4. Resolve runtime and closure
5. Validate contract and capability requirements
6. Materialize binding
7. Execute or persist

This state machine defines the preconditions for entering the existing consumer flow or durable workspace materialization. It does not replace the downstream hourglass vocabulary.

Completion of canonical lock synthesis means that downstream phases consume a canonical lock model, whether fully resolved or explicitly partially resolved. It does not require immediate on-disk persistence for `run`.

Terminal behavior differs by command.

For `run`:

- step 7 means generate execution artifacts for the attempt and execute
- lock persistence is optional

For `init`:

- step 7 means materialize durable workspace state into the target workspace directory and persist project state
- execute is optional follow-up behavior, not the primary command contract

## 8. Relation To Hourglass Pipeline

This ADR preserves the bidirectional hourglass model.

It does not redefine:

- consumer flow phase order
- producer flow phase order
- shared phase semantics

Instead, it defines how source inputs become canonical lock state before downstream execution proceeds.

More precisely, this ADR defines preconditions for entering the consumer flow when the starting input is source rather than an already materialized canonical lock.

For `ato run`, source inference and resolution must complete early enough that the rest of the consumer flow operates on canonical lock-derived execution input rather than ad hoc source heuristics.

The practical mapping is:

- source acquisition and lock detection happen before or at the start of Install/Prepare responsibilities
- canonical lock synthesis must complete before downstream Verify and Dry-run rely on execution semantics
- binding materialization must happen before Execute

This ADR deliberately avoids changing the existing hourglass vocabulary.

## 9. Field-Level Inference Rules

### 9.1 contract.process

This is the highest-priority inference target.

Priority order:

1. existing `ato.lock.json`
2. explicit ecosystem-declared command or entrypoint
3. framework default
4. well-known file heuristics
5. unresolved

Examples:

- Node: `package.json` `scripts.start`, then `scripts.dev`, then framework default
- Python: `pyproject.toml` scripts, then framework conventions such as FastAPI, Django, Flask
- Rust: `Cargo.toml` bin target, then web server crate evidence as a secondary hint

Rules:

- `run` and `init` must share the same deterministic inference rules
- `run` may allow wider fallback for immediate execution
- `init` may preserve unresolved state rather than locking in a weak guess
- if a strict total order exists, ato may choose by deterministic precedence
- if multiple equal-ranked candidates remain, `ato run` must require explicit selection, confirmation, or fail
- if multiple equal-ranked candidates remain, `ato init` must require explicit selection or persist an explicit unresolved marker
- inference here targets the selected primary executable unit; repositories with multiple equal-ranked executable units must surface selection rather than silently auto-pick
- future `workloads` inference may generalize this model beyond a single primary executable unit

### 9.2 contract.network

Inference must populate the application-expected port and protocol, not the actual host allocation.

This means:

- expected ingress port belongs in `contract`
- actual host port belongs in `binding`

Examples of framework defaults:

- Vite -> 5173
- Next.js -> 3000
- Astro -> 4321
- Django -> 8000

Rules:

- default port inference is allowed
- host port conflicts are binding problems, not contract problems
- ato must not rewrite the contract just because the local host cannot bind the preferred port

### 9.3 contract.env_contract

Environment variable inference must be conservative.

Priority order:

1. `.env.example`
2. `.env.template`
3. documented config files
4. framework conventions
5. optional static code scan
6. runtime prompt

Rules:

- AST or grep based scans are hints, not authoritative contract evidence
- required/optional classification must prefer explicit evidence
- weakly inferred env variables may appear in diagnostics or suggestions without being promoted into durable contract state

### 9.4 contract.filesystem

Filesystem inference may identify likely cache, config, temp, and persistent candidates.

Examples:

- `node_modules`, `.pnpm-store`, `target`, `__pycache__` -> cache or ephemeral
- `.env`, config files -> config/read
- sqlite databases, uploads, user data directories -> persistent candidates

Rules:

- cache classification may be moderately aggressive
- persistent write requirements must be conservative
- uncertain write requirements may remain unresolved or move into runtime observation and attestation

### 9.5 resolution.runtime

Runtime version inference should be evidence-driven.

Priority order:

- `.tool-versions`
- `.nvmrc`
- `package.json` `engines`
- `.python-version`
- `pyproject.toml` `requires-python`
- `rust-toolchain.toml`
- ecosystem lock metadata
- Ato default LTS fallback

Rules:

- default fallback is allowed
- fallback usage must be surfaced in diagnostics and inspect output
- `init` should surface fallback more explicitly than `run`
- durable output produced via fallback must retain provenance that fallback was used

### 9.6 resolution.closure_digest

Closure digest computation is primarily a resolution concern, not a heuristic inference concern.

Rules:

- existing ecosystem lockfiles must be used when available
- sandbox-assisted completion is allowed only when lockfiles are absent or incomplete and the operation is permitted
- closure completion must happen before a durable lock is persisted by `init`
- `run` may resolve closure ephemerally for the current attempt

This ADR therefore distinguishes infer from resolve.

- infer derives the lock skeleton
- resolve completes runtime, dependency, and target materialization state

## 10. Provenance And Diagnostics

Inference output should preserve provenance metadata for diagnostics and inspection, even if that metadata is not part of the canonical hashed projection.

Durable state produced by `ato init` must retain inspectable provenance for inferred and resolved durable fields, even when that provenance is stored outside the canonical hash projection.

Examples:

- `contract.network.ingress[0].port` inferred from `package.json` plus a framework rule
- `resolution.runtime.node.version` inferred from `.nvmrc`
- `resolution.closure_digest` computed from a sandbox-generated ecosystem lockfile

Diagnostics and inspect surfaces should be able to answer:

- where this field came from
- whether it was explicit, inferred, resolved, observed, or user-confirmed
- whether a security approval or consent gate was involved
- whether fallback was used

## 11. Persistence Rules

### 11.1 run persistence

`ato run` may cache ephemeral inference artifacts, but by default it must not write repo-tracked `ato.lock.json`.

Allowed default writes include:

- workspace-local ephemeral lock cache
- workspace-local binding cache
- temporary generated ecosystem lockfiles in Ato-managed working directories
- provenance traces and diagnostics

Repo-tracked lock write-back is opt-in only.

When enabled, run write-back should remain cache or artifact-promotion oriented. It must not replace `ato init` as the mechanism for durable unresolved baseline emission or workspace directory bootstrap.

### 11.2 init persistence

`ato init` must persist enough state to establish a durable development baseline.

`ato init` owns durable workspace-state materialization. For local source this means materializing the durable baseline into the current directory when that directory is the selected workspace target. For remote or newly acquired source this means materializing the durable baseline into the target workspace directory chosen by acquisition semantics.

Minimum durable outputs:

- `ato.lock.json`

Optional additional outputs:

- workspace-local binding seed
- workspace-local provenance cache
- workspace-local Ato state under Ato-managed directories

### 11.3 Binding persistence

`binding` remains host-local by default. Even when `init` persists workspace state, actual host allocation details should remain outside repo-tracked canonical lock content unless explicitly opted in.

A workspace-local binding seed may record preferred local development intent or previously approved non-portable defaults, but it must not be treated as repo-tracked canonical execution state.

## 12. Remote Source Initialization

`ato init <source>` is defined as initializing an Ato-managed workspace from source.

For local source:

- analyze the current directory
- infer and resolve canonical lock state
- write `ato.lock.json`
- initialize optional workspace-local Ato state

For remote source such as `github.com/owner/repo`:

- acquire source
- infer and resolve canonical lock state
- write workspace `ato.lock.json`
- initialize optional workspace-local Ato state

This makes `init` the durable onboarding command under a lock-first architecture.

This ADR defines lock semantics, not acquisition UX. Remote acquisition must produce a local source tree before inference begins, and subsequent inference follows the same evidence and fail-closed rules as local source.

Remote source acquisition should preserve fetched source identity strongly enough for later diagnostics and deterministic re-validation, for example by retaining repository URL and commit or ref provenance in `resolution`-adjacent provenance state.

Remote acquisition semantics such as clone destination, empty-directory requirements, overwrite policy, and workspace ownership remain out of scope for this ADR.

## 13. Non-Goals

This ADR does not define:

- the full JSON Schema for `ato.lock.json`
- the final storage format for provenance metadata
- the full approval UX for secrets and identity
- organization policy authoring and distribution
- automatic source code repair as part of inference

This ADR also avoids collapsing inference and repair into one concept. Inference reads and derives meaning. Repair resolves missing pieces, closure completion, or conflicts under bounded rules.

## 14. Consequences

### Positive consequences

- lock-first execution becomes possible without requiring hand-authored lock files
- `run` remains convenient for source-only workflows
- `init` becomes the durable workspace-state materialization command instead of a manifest prompt generator
- contract and binding remain separated during inference
- provenance-aware diagnostics become possible

### Costs and trade-offs

- the inference engine becomes a central subsystem with significant rule maintenance cost
- framework heuristics must be curated and versioned carefully
- conservative env and filesystem inference may leave some projects partially unresolved
- sandbox-assisted closure completion requires tight containment and clear failure behavior

### Primary design constraints

- `run` must not silently become a project initializer
- host conflicts must be solved in binding, not by mutating contract intent
- security-sensitive fields must fail closed rather than being guessed from weak evidence

## 15. Canonical Summary

This ADR defines how ato-cli reaches canonical lock state from source-only inputs.

- `ato run` performs ephemeral, attempt-scoped materialization for execution
- `ato init` performs durable, workspace-scoped materialization
- inference prefers deterministic portable core fields
- resolution completes runtime and closure state
- host-local data is deferred to binding
- the existing hourglass pipeline remains intact

Under this model, lock-first does not mean source-hostile. It means source must first be converted into canonical lock state before execution or durable workspace materialization proceeds.
