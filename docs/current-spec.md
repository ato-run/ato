# ato-cli Current Specification

This document consolidates the current target product specification of ato-cli into a single file.
It is the canonical target spec for the unified run/publish pipeline; implementation may temporarily lag in some areas during migration.

- Scope: target product surface after bidirectional hourglass pipeline unification
- Baseline: derived from the ato-cli 0.4.30 product surface
- Canonical input: `ato.lock.json`
- Compatibility authoring/input: `capsule.toml`, `capsule.lock.json`, source-only directories, ecosystem lockfiles, framework metadata
- Canonical binary: ato
- Security posture: Zero-Trust, fail-closed by default

## 1. Product Summary

ato is a meta-CLI that executes, packages, publishes, installs, and inspects applications through a lock-first model centered on `ato.lock.json`.
This specification is intentionally normative for the next pipeline model, not a strict snapshot of the current implementation.

The target product surface is organized around these flows:

1. Local execution: ato run
2. Packaging: ato build
3. Distribution: ato publish
4. Installation: ato install
5. Discovery and inspection: ato search, ato inspect requirements
6. Runtime and launcher management: ato ps, ato stop, ato logs, ato project, ato unproject

Core behavior is intentionally fail-closed:

- missing manifest requirements stop execution
- missing runtime lockfiles stop execution
- unsupported or ambiguous runtime resolution stops execution
- permission bypass requires explicit dangerous flags and environment gating
- verification is mandatory on normal install paths

## 2. Canonical Concepts

### 2.1 Capsule

A capsule is the unit of execution and distribution. It may be represented by authoritative lock state, compatibility manifest input, or an immutable `.capsule` archive depending on the command entry path.

### 2.2 Canonical Input And Compatibility Inputs

`ato.lock.json` is the only canonical input for execution semantics.

- when `ato.lock.json` exists, it is the authoritative source of `contract` and `resolution`
- `capsule.toml`, legacy `capsule.lock.json`, source-only directories, ecosystem lockfiles, and framework metadata are compatibility or importer inputs
- compatibility inputs may be used to synthesize canonical lock-shaped state, but they must not override an authoritative `ato.lock.json`
- parser compatibility for schema versions 0.2, 0.3, and 1 remains relevant only inside compatibility import and normalization paths
- some CHML-like manifests without `schema_version` are still normalized internally through that same compatibility path

### 2.3 Security Model

ato is designed around a Zero-Trust / fail-closed model.

- Success paths stay quiet when possible
- Consent prompts are explicit
- Policy violations are surfaced explicitly
- Non-interactive automation must opt in with flags such as -y when consent would otherwise be required

### 2.4 Bidirectional Hourglass Pipeline

ato defines a shared internal phase vocabulary for consumer and producer flows.

Consumer flow (run):

1. Install
2. Prepare
3. Build
4. Verify
5. Dry-run
6. Execute

Producer flow (publish):

1. Prepare
2. Build
3. Verify
4. Install
5. Dry-run
6. Publish

Common phase semantics:

- Install means a clean artifact installation into a concrete directory target so later phases operate on an installed environment; consumer and producer flows share this same semantic contract, and only the destination context differs. In consumer flow the installed environment becomes the execution root, while in producer flow the installed environment becomes the dry-run validation root. Install does not mean local CAS registration.
- Prepare means source diagnosis, shadow workspace construction, auto-provisioning, and synthetic environment setup needed to continue fail-closed flows
- Build means JIT or packaging-time build work required before verification or terminal execution/distribution
- Verify means manifest, signature, hash, lockfile, and policy validation before side effects
- Dry-run means side-effect-free preflight simulation, permission checks, and registry/runtime readiness checks
- Execute means launching the sandboxed or policy-approved runtime process
- Publish means deploy the verified artifact to the selected destination such as Local CAS, Store, or a remote registry

Failure and abortability rules:

- every phase is fail-closed and must either commit a complete result or leave no partial state behind
- temporary directories, extracted payloads, sidecars, launched helper processes, and synthetic workspaces created by Install, Prepare, Build, Verify, or Dry-run must be registered for cleanup before the phase can report success
- on phase failure, ato runs compensating cleanup for all uncommitted resources created in the current pipeline attempt and returns to a safe baseline state before surfacing the final error
- cleanup failure does not mask the primary error; the primary error remains authoritative and cleanup failures are appended as secondary causes or diagnostics
- failure handling is phase-aware: manifest or inference failures are classified separately from source/build/runtime failures so remediation can be targeted

Public CLI phase-selection rules:

- Public CLI flags remain coarse-grained rather than exposing every phase directly
- For `publish`, `--prepare`, `--build`, and `--deploy` select stop points over this shared vocabulary

### 2.5 Ecosystem Importer Boundary

ato treats ecosystem lockfiles and framework metadata as read-only importer evidence, not as Ato-native canonical truth.

- importer evidence currently covers `uv.lock`, `package-lock.json`, `pnpm-lock.yaml`, `yarn.lock`, `bun.lock`, `deno.lock`, `Cargo.lock`, `go.sum`, and `poetry.lock`
- importer evidence also covers native-delivery framework metadata for Tauri, Electron, and Wails
- importer output is canonicalization input used to build `resolution.closure`, `build_closure.inputs`, preflight lockfile decisions, and native build cache inputs
- importer output does not directly define `lock_id` or `closure_digest`; those are computed only after ato normalizes canonical lock-shaped state
- importer probing is read-only; ato does not run package managers or framework CLIs as part of importer observation
- transitional helpers such as `generate_uv_lock`, `generate_pnpm_lock`, and `generate_deno_lock` keep their public names but now behave as evidence wrappers that either find required importer evidence or fail closed with manual-generation guidance

### 2.6 Bootstrap Trust Boundary

ato distinguishes three bootstrap authorities.

- `locked_artifact`: a runtime or tool artifact declared in lock data and installed into a durable user cache with checksum verification
- `network_bootstrap`: a network fetch used to acquire a helper such as `uv`, `pnpm`, or `nacelle`; this remains policy-gated and cache-backed rather than becoming canonical lock content
- `host_capability`: a host-local executable that must already exist on `PATH` for authoritative lock-derived source execution or native finalize/projection

Current boundary rules:

- lock-derived source execution prefers `host_capability` for host runtimes/tools such as `node`, `python`, `deno`, and `uv`
- compatibility/runtime-manager install paths use `locked_artifact` when `capsule.lock.json` carries runtime or tool artifacts plus checksums
- `nacelle` auto-bootstrap is a `network_bootstrap` path with a typed network policy derived from environment configuration
- native-delivery finalize helpers such as `codesign` and `signtool` are `host_capability`, but they may still appear in `build_environment.helper_tools` as build-environment claims rather than as canonical closure artifacts
- durable bootstrap caches live under `~/.ato/runtimes`, `~/.ato/toolchains`, and `~/.ato/engines`; transient download/extract staging remains under `.downloads` and `.tmp` inside those durable roots

### 2.7 Command-Level Reproducibility Contract

ato evaluates lock-first behavior command by command rather than by architecture purity alone.

| Command   | Must be fixed before success                                                                                                                                                                                      | May remain unresolved                                                                        | Host-local / non-canonical state                                                        | Provenance / diagnostics requirement                                                                                            |
| --------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------- |
| `run`     | immutable execution input, runtime selection, process entry, required closure materialization, expected network/binding contract, security gate verdict                                                           | descriptive metadata, optional hints, non-required env/config hints                          | actual host ports, secret values, local paths, approval results, binding selections     | unresolved fields must indicate whether they block execute; import, fallback, and host-local influence must be machine-readable |
| `init`    | durable `ato.lock.json`, runtime/toolchain baseline, resolved target selection or explicit unresolved marker, executable process contract or explicit unresolved marker, delivery build contract where applicable | partially resolved fields that still support deterministic re-validation                     | workspace-local binding seed, policy bundle, attestations store, approval results       | fallback, importer evidence, observation, and user-confirmed information must remain inspectable after generation               |
| `publish` | build closure, artifact identity class, publish metadata, provenance linkage, source-derived build/verify/publish path                                                                                            | optional descriptive metadata that does not change artifact identity or verification outcome | local finalize credentials, local projection/install state, registry session/auth state | metadata must preserve whether the artifact is source-derived unsigned, locally finalized signed, or imported third-party input |

Shared rules:

- authoritative `ato.lock.json` must prevent downstream manifest or ad hoc source heuristics from changing command semantics
- unresolved state must carry explicit reason classes rather than disappearing into fallback
- import, fallback, and host-local overlays must remain inspectable through machine-readable surfaces such as `inspect`, `validate`, and diagnostics
- `closure_digest` and `lock_id` must not be used to claim more reproducibility than the resolved `resolution.closure.kind/status` envelope supports

### 2.8 Publish Upload Strategy Boundary

The publish pipeline separates upload execution into a strategy boundary so transport changes do not leak into CLI orchestration.

- upload execution is modeled as three explicit stages: `start_upload`, `transfer`, and `finalize_upload`
- `start_upload` selects or prepares the transport-specific upload target and any required request metadata
- `transfer` performs the byte movement for the chosen strategy
- `finalize_upload` commits or confirms the remote publish result and any metadata synchronization needed after transfer
- the current selector is centralized and hides transport policy behind a single strategy selection point
- current behavior still defaults to direct upload for both managed Store hosts and custom registries
- a presigned upload strategy may exist as a non-default skeleton before the remote capability is enabled in production
- host-based selection is transitional; the target end-state is capability-based discovery rather than hard-coded host behavior

## 3. Supported CLI Surface

## 3.1 Primary Commands

### run

Runs a local project, a local .capsule file, an installed store capsule, or a GitHub repository shorthand.

Internal Pipeline Phases (Consumer Flow):

1. Install: resolves or fetches the selected artifact when needed and unpacks it into an execution directory; skipped for purely local source inputs that already provide a working tree
2. Prepare: constructs shadow workspaces when needed, performs auto-provisioning, and injects synthetic environments required for fail-closed continuation
3. Build: performs JIT or pre-execution build steps, including runtime-specific lifecycle builds when required
4. Verify: validates manifest structure, lockfile integrity, external dependency metadata, and security policy inputs before launch
5. Dry-run: performs preflight checks such as consent gating, environment validation, and other no-side-effect launch checks
6. Execute: launches the sandboxed or policy-approved process

Accepted inputs:

- local directory
- local capsule.toml
- local .capsule file
- scoped store ID in publisher/slug form
- GitHub shorthand in github.com/owner/repo form

Important rules:

- github.com/owner/repo is the canonical GitHub run syntax
- https://github.com/... and other non-canonical GitHub URL forms are rejected for ato run and produce a corrective error
- slug-only inputs are rejected; the CLI prompts toward publisher/slug
- if a scoped capsule is not installed, ato can auto-install it
- JSON mode requires -y for auto-installing missing capsules in non-interactive flows
- if a local directory or local manifest path does not resolve to a valid capsule.toml, ato pauses normal execution and offers to generate a new capsule.toml through the existing init flow
- if consent is granted or -y is passed, the generated manifest is written to the local project root before the run pipeline continues
- if an invalid capsule.toml already exists, ato first backs it up under `.tmp/ato/run-invalid-manifests/` before regeneration
- when `ato run publisher/slug` resolves to an installed desktop-native capsule that has already completed host-local derivation, Execute launches the locally derived app bundle through the platform launcher instead of supervising the extracted runtime-tree bundle as a long-running service
- for that desktop-native open-only path, `--background` records the launch request and returns success without waiting for service-style readiness events

Current lock-first contract:

- source-started run must synthesize attempt-local canonical lock-shaped state before Execute
- once immutable execution input is materialized for the attempt, active execution semantics must not continue consulting ad hoc source heuristics
- process entry, selected runtime, target compatibility, required closure materialization, security-sensitive capability decisions, and expected network/binding contract are execute-blocking fields
- host-local selections such as actual bound ports, secret values, local paths, and approval results remain outside canonical lock identity
- imported desktop artifacts may run through a provenance-limited `artifact-import` path, but that path does not claim reproducible rebuild semantics

Migration note:

- `run` still retains transitional manifest-path surfaces for process records, shadow workspaces, and preview-session compatibility
- these surfaces are not intended to override authoritative lock semantics, but they have not yet been fully removed from run/install-adjacent plumbing

Important flags:

- --target <label>
- --watch
- --background
- --registry <url>
- --state NAME=/abs/path or NAME=state-...
- --inject KEY=VALUE
- --enforcement strict|best-effort
- --sandbox
- -U, --dangerously-skip-permissions
- --compatibility-fallback host
- -y, --yes
- --agent auto|off|force
- --allow_unverified is not the flag name; the actual flag is --allow-unverified

### install

Installs from a registry by scoped ID or directly from a public GitHub repository.

Accepted forms:

- ato install publisher/slug
- ato install --from-gh-repo github.com/owner/repo

Important rules:

- --registry cannot be combined with --from-gh-repo
- --version cannot be combined with --from-gh-repo
- --skip-verify is a hidden legacy flag and is always rejected
- verification is not an optional normal-path feature anymore
- launcher projection can be forced, skipped, or prompted

Important flags:

- --registry <url>
- --version <semver>
- --default
- -y, --yes
- --allow-unverified
- --output <dir>
- --project
- --no-project
- --json

Current contract:

- install remains verification-first and lock-first for artifact semantics
- launcher projection, preview/manual recovery, and some persistence paths still retain transitional `source_manifest_path` plumbing during migration
- those transitional paths are compatibility surfaces for install/projection workflows and are not intended to redefine canonical execution semantics
- for desktop-native installs that require host-local derivation, the launchable surface is the derived app under `~/.ato/apps/.../derived-*`, not the immutable `.capsule` under `~/.ato/store`

### init

Materializes a durable workspace baseline.

Current contract:

- writes `ato.lock.json` as the primary durable output
- initializes workspace-local `.ato/source-inference/provenance.json`
- initializes workspace-local `.ato/source-inference/provenance-cache.json`
- initializes workspace-local `.ato/binding/seed.json`
- initializes workspace-local `.ato/policy/bundle.json`
- initializes workspace-local `.ato/attestations/store.json`
- `resolution.closure` uses a normalized envelope with `kind` and `status`; metadata-only closure state is explicit and remains incomplete until closure completion materializes a digestable closure
- metadata-only closure observation records importer labels in `observed_lockfiles`; concrete filesystem paths remain provenance-side evidence
- `build_closure.build_environment` uses array-based categories: `toolchains`, `package_managers`, `sdks`, and `helper_tools`
- compatibility import may emit `imported_artifact_closure` for an existing native artifact such as a `.app` bundle; this remains provenance-limited and distinct from source-derived build closure
- canonical lock identity remains `schema_version + resolution + contract`; embedded `binding`, `policy`, `attestations`, and `signatures` do not affect `lock_id`
- may persist partially resolved output, but unresolved state must remain inspectable through first-class markers and provenance metadata
- top-level `ato init` only materializes durable `ato.lock.json` workspace state; compatibility `capsule.toml` scaffolding remains on `ato build --init` and local manifest recovery paths
- `ato init` is the primary command for desktop native-delivery toml レス化; `run` and `publish` should consume its durable baseline rather than rediscovering source semantics
- fallback, importer evidence, observation, and user-confirmed information must survive as inspectable provenance rather than staying implicit in heuristics

### build

Packages a project into a .capsule archive.

Aliases:

- pack

Important flags:

- [dir]
- --init
- --key <path>
- --enforcement strict|best-effort
- --standalone
- --force-large-payload
- --keep-failed-artifacts
- --timings
- --strict-v3

Current strictness contract:

- ato build --strict-v3 disables fallback when source_digest or CAS(v3 path) is unavailable
- use it when build diagnostics must fail immediately instead of falling back to a looser path

### publish

Publishes capsule artifacts to a registry.

Implementation status during migration:

- phase selection, stop-point validation, and phase ordering are already owned by application::pipeline::producer
- the CLI entry is wired through cli::dispatch::publish, which hosts the phase runner integration for the publish command
- application::pipeline::phases::publish already owns the wrapper APIs for summarize_private_publish, run_private_publish_phase, and run_official_publish_phase
- private remote registry upload is now driven through DestinationPort in the main publish path
- build-backed private publish now resolves source vs artifact input in application::pipeline::phases::publish before handing off to that same upload boundary

Current lock-first contract:

- source input must enter publish through lock-derived build / verify / publish planning rather than ad hoc source semantics
- artifact identity must distinguish source-derived unsigned bundles, locally finalized signed bundles, and imported third-party artifacts
- publish metadata and provenance must preserve that identity class instead of collapsing all desktop artifacts into one bucket
- imported artifact publish remains provenance-limited and must not claim source-derived rebuild reproducibility

Internal Pipeline Phases (Producer Flow):

1. Prepare: diagnoses the source, prepares reproducible inputs, and generates any pre-build material needed for a deterministic artifact
2. Build: compiles assets and packs the .capsule archive when source input is used
3. Verify: calculates hashes, validates policy constraints, and verifies the artifact before distribution
4. Install: unpacks the verified artifact into a temporary test sandbox so later producer phases validate the installable result; this is not the public `ato install` command
5. Dry-run: simulates registry communication, permission checks, and upload readiness without external side effects
6. Publish: deploys the verified artifact to the selected destination such as Local CAS, Store, or a remote registry

Current prepare-stage behavior for source-backed publish:

- lockfile-backed dependency resolution belongs to `Prepare`, not `Build`
- `Prepare` may run one or more dependency materialization steps such as `npm ci`, `yarn install --frozen-lockfile`, `pnpm install --frozen-lockfile`, `bun install --frozen-lockfile`, `uv sync --frozen`, `cargo fetch --locked`, or `cargo generate-lockfile` when native source evidence is present but `Cargo.lock` is still missing
- when both ecosystems are present, such as Tauri source with Node and Cargo lockfiles, `Prepare` may execute multiple dependency-resolution steps before any explicit prepare lifecycle hook
- explicit lifecycle prepare hooks such as `build.lifecycle.prepare` or `package.json` `scripts["capsule:prepare"]` run after lockfile-backed dependency resolution

Phase selection rules:

- public flags remain `--prepare`, `--build`, and `--deploy`; they select stop points over the shared phase vocabulary rather than exposing every phase directly
- stop points are fixed: `--prepare => Prepare`, `--build => Verify`, and `--deploy => Publish`
- official registries default to `Publish` only when no explicit phase flag is provided
- Personal Dock, private registries, and local registries default to `Prepare` through `Publish` when no explicit phase flag is provided
- `--artifact` changes the start phase to `Verify`, skipping `Prepare` and `Build`
- `official + --deploy` remains `Publish` only, preserving CI-first handoff semantics
- `private/local + --deploy` can resolve required earlier phases from source input automatically; with `--artifact`, it runs from `Verify` through `Publish`
- `--artifact --build` runs `Verify` only because the selected stop point is `Verify`
- `--artifact --prepare` is invalid because the start phase would occur after the selected stop point
- --ci and --dry-run cannot be combined with phase flags

Important flags:

- --registry <url>
- --artifact <path>
- --scoped-id <publisher/slug>
- --allow-existing
- --prepare
- --build
- --deploy
- --legacy-full-publish
- --force-large-payload
- --fix
- --ci
- --dry-run
- --no-tui
- --json

Registry mode behavior:

- Official registry, such as https://api.ato.run: CI-first, `Publish` only by default, no normal direct local upload
- Personal Dock: default target when logged in and no registry is specified
- Custom or private registry: direct upload flow, defaulting to `Prepare` through `Publish` unless narrowed by stop point or artifact input
- /d/<handle> is a public UI page, not a registry URL

Compatibility note:

- --legacy-full-publish is official-only, deprecated, and scheduled for removal in the next major release

### search

Searches the store for packages.

Important flags:

- [query]
- --category <value>
- --tag a,b,c
- --limit <n>
- --cursor <token>
- --registry <url>
- --json
- --no-tui
- --show-manifest

## 3.2 Management Commands

### ps

Lists running capsules.

Flags:

- --all
- --json

### stop

Stops a running capsule.

Alias:

- close

Flags:

- --id <capsule-id>
- --name <name>
- --all
- --force

### logs

Shows logs of a running capsule.

Flags:

- --id <capsule-id>
- --name <name>
- --follow
- --tail <n>

### state

Persistent state binding management.

Subcommands:

- list
- inspect
- register

Current register contract:

- manifest defaults to .
- state name is explicit via --name
- host path must be absolute via --path /ABS/PATH

### binding

Host-side service binding management.

Subcommands:

- list
- inspect
- resolve
- bootstrap-tls
- serve-ingress
- register-ingress
- register-service
- sync-process

Notable current capabilities:

- binding resolution by owner_scope + service_name + binding_kind
- TLS bootstrap for binding references
- ingress registration and serving
- process sync for service metadata

## 3.3 Auth Commands

### login

Logs in to Ato registry.

Flags:

- --token <token>
- --headless

### logout

Logs out.

### whoami

Shows current authentication status.

Alias:

- auth

Auth source precedence:

1. ATO_TOKEN
2. OS keyring
3. ${XDG_CONFIG_HOME:-~/.config}/ato/credentials.toml
4. legacy ~/.ato/credentials.json as read-only fallback

## 3.4 Advanced Commands

### validate

Validates capsule build/run inputs without executing.

### inspect requirements

Returns a stable machine-readable requirements contract derived from capsule.toml.

Current guarantees:

- capsule.toml is the only source of truth for requirement discovery
- local paths and publisher/slug refs return the same top-level JSON shape
- state-related requirements are exposed under requirements.state
- success prints JSON to stdout
- JSON failures print structured JSON and exit non-zero

### fetch

Fetches an artifact into local cache for debugging or manual workflows.

### finalize

Performs local derivation for a fetched native artifact.

### project

Adds a finalized app to launcher surfaces.

Subcommand:

- ls

### unproject

Removes an experimental launcher projection.

### key

Signing key management.

Subcommands:

- gen
- sign
- verify

### config

Configuration management.

Subcommands:

- engine features
- engine register
- engine install
- registry resolve
- registry list
- registry clear-cache

### gen-ci

Generates the fixed GitHub Actions workflow for OIDC CI publish.

### registry

Registry management.

Subcommands:

- resolve
- list
- clear-cache
- serve

Current serve defaults:

- host: 127.0.0.1
- port: 8787
- data_dir: ~/.ato/local-registry

Operational note:

- local verification examples in this repository commonly use port 18787 to avoid collisions with other app services

## 3.5 Hidden and Compatibility Commands

The following commands exist in the CLI surface but are hidden from normal help because they are compatibility, internal, or expert-oriented workflows:

- setup
- new
- keygen
- scaffold docker
- sign
- verify
- profile
- package
- source
- guest
- ipc
- engine

Current hidden command highlights:

- setup is a compatibility alias for engine installation behavior
- source provides sync-status and rebuild for source-backed registry workflows
- ipc provides status, start, stop, and invoke for JSON-RPC style IPC services
- scaffold docker emits Docker-oriented scaffolding from capsule.toml

## 4. Input Resolution Rules

## 4.1 run Input Resolution

ato run resolves input in this order:

1. local path after path expansion
2. GitHub shorthand in github.com/owner/repo form
3. scoped capsule reference in publisher/slug form

If input is local:

- directories run as local projects
- capsule.toml can be addressed directly
- .capsule archives run as packaged artifacts
- if no valid capsule.toml is found for a local directory or local manifest target, ato pauses resolution and requests consent to generate or repair one via the interactive init flow
- with consent or -y, ato writes the generated capsule.toml into the local project root and then resumes the run pipeline
- if the existing file is invalid, ato backs it up before regeneration
- without a TTY and without -y, ato fails closed instead of continuing

If input is GitHub shorthand:

- ato prepares a GitHub preview session
- ato can infer a runnable manifest for install and execution
- non-canonical GitHub URL forms are rejected with a corrective message

If input is publisher/slug:

- ato prefers an installed matching capsule when possible
- with explicit --registry, ato can compare against registry current version
- if the capsule is missing locally, ato may auto-install it after consent

## 4.2 install Input Resolution

ato install supports two families of sources:

1. store registry content via publisher/slug
2. public GitHub repositories via --from-gh-repo

For store references:

- slug-only references are rejected
- publisher and slug must be lowercase kebab-case
- optional version can be specified

For GitHub sources:

- the repository is normalized first
- an install draft is fetched from the store API
- preview TOML may be normalized further from the checked-out repository contents

## 5. Manifest Specification

For the strict generation contract used by automated producers such as Store-side GitHub inference, see docs/capsule-toml-generation-spec.md.

## 5.1 Top-Level Contract

The canonical current form is a capsule.toml file.

Minimal v0.2 example:

```toml
schema_version = "0.2"
name = "example-app"
version = "0.1.0"
type = "app"
default_target = "cli"

[targets.cli]
runtime = "source"
driver = "deno"
runtime_version = "1.46.3"
entrypoint = "main.ts"
```

Current top-level fields in the normalized model:

- schema_version
- name
- version
- type
- default_target
- metadata
- capabilities
- requirements
- storage
- state
- state_owner_scope
- service_binding_scope
- routing
- network
- model
- transparency
- pool
- build
- pack
- isolation
- polymorphism
- targets
- services
- distribution

## 5.2 Validation Rules

Current validation rules include:

- supported schema_version values: 0.2, 0.3, 1
- schema_version = "1" is canonical; schema_version = "0.2" is deprecated compatibility input
- name must be kebab-case
- name length must be between 3 and 64
- non-empty version must be semver
- non-library manifests require default_target
- non-library manifests require at least one target
- default_target must exist in [targets.<label>]
- pack.include and pack.exclude cannot contain empty patterns
- requirements memory strings must parse correctly where present
- inference capsules require capabilities
- inference capsules require model config
- empty state_owner_scope is invalid
- empty service_binding_scope is invalid

## 5.3 Targets

Current v0.2 target declaration lives under [targets.<label>].

Named target fields currently supported in the normalized model:

- runtime
- driver
- language
- runtime_version
- runtime_tools
- entrypoint
- run
- image
- cmd
- env
- required_env
- public
- port
- working_dir
- package_type
- build_command
- outputs
- build_env
- run_command
- readiness_probe
- package_dependencies
- external_dependencies
- external_injection

Allowed runtime values:

- source
- web
- wasm
- oci

Allowed driver values by current validation path:

- static
- deno
- node
- python
- wasmtime
- native

## 5.4 Runtime-Specific Rules

### source targets

Current rules:

- entrypoint or run_command is required in canonical normalized output
- entrypoint, run_command, or run is accepted in compatibility input
- for driver deno, node, or python in non-preview v0.2 validation, runtime_version is required
- driver can be inferred from language and entrypoint in some flows

### web targets

Current rules:

- driver is required and must be one of static, node, deno, python
- port is required outside preview mode
- port must be between 1 and 65535
- public is deprecated and rejected for runtime=web
- entrypoint or run_command is required unless the target is in web services mode
- compatibility input may provide run, which is normalized before execution
- entrypoint = "ato-entry.ts" is deprecated and rejected in web services mode

### oci targets

Current rules:

- image or entrypoint is required

### library targets

For schema v0.3 library targets normalized into the current model:

- runtime execution fields must not be defined
- build/package metadata can be preserved

## 5.5 Services Mode

Top-level [services] enables supervisor-style multi-service orchestration.

Service fields currently supported:

- entrypoint
- target
- depends_on
- expose
- env
- state_bindings
- readiness_probe
- network

Current repository guidance:

- use a single web/deno target with top-level [services] for dynamic multi-service apps
- ato validates the services graph up front and materializes a ServiceGraphPlan before service startup
- ato uses ServicePhaseCoordinator inside the services executors to enforce DAG layers during startup
- ato starts services in DAG layer order and waits on readiness probes before releasing dependent layers
- service start within a layer is currently serial; readiness waiting within a layer is parallel and fail-fast
- ato prefixes logs
- ato fail-fast stops all services when one exits
- services.main is required by the documented services-mode recipe

Services orchestration rules:

- the services graph is validated as a DAG before execution; dependency cycles are rejected before any service enters Execute
- the current implementation does not yet run Install, Prepare, Build, Verify, and Dry-run as graph-wide per-service barriers; those phases still belong to the normal run/publish pipelines outside services startup coordination
- Execute startup is graph-aware: web_services and orchestrator both share ServiceGraphPlan plus ServicePhaseCoordinator for dependency ordering
- each DAG layer is started in order; after the layer has been spawned, readiness waits run concurrently for the services in that layer
- readiness failure is treated as a service execution failure and triggers fail-fast shutdown of the whole graph
- if one running service exits unexpectedly, ato terminates the remaining services, collects exit causes, and reports the graph as failed
- startup side effects such as child-process registration are committed to the attempt cleanup journal before readiness waiting so fail-fast abort can still unwind safely

## 5.6 State and Storage

### state

State declarations are filesystem-backed application state requirements.

Current state field model:

- kind: currently filesystem
- durability: ephemeral or persistent
- purpose
- producer
- attach: auto or explicit
- schema_id

### storage

Persistent storage model includes volumes with:

- name
- mount_path
- read_only
- size_bytes
- use_thin
- encrypted

## 5.7 Network and Isolation

### network

Current egress model includes:

- network.egress_allow for L7 or proxy allowlists
- network.egress_id_allow for IP, CIDR, or spiffe-style identifiers

### isolation

Current isolation model includes:

- isolation.allow_env for explicit host environment passthrough

## 5.8 Build and Pack Metadata

### build

Current packaging-time build metadata includes:

- exclude_libs
- gpu
- lifecycle
- inputs
- outputs
- policy

Lifecycle keys:

- prepare
- build
- package
- verify
- publish

These lifecycle keys are manifest metadata for packaging/build orchestration. They are not the same thing as the internal run/publish pipeline phases even when names overlap.

### pack

Current packaging filter metadata includes:

- include
- exclude

pack.include is a strict allowlist when specified.

## 5.9 Distribution Metadata

distribution is generated at pack or publish time and may include:

- manifest_hash
- merkle_root
- chunk_list
- signatures

## 6. Runtime Isolation Tiers

Current documented runtime policy is:

- web/static: Tier1
- web/deno: Tier1
- web/node: Tier1 through Deno compat execution
- web/python: Tier2
- source/deno: Tier1
- source/node: Tier1 through Deno compat execution
- source/python: Tier2
- source/native: Tier2

Current lockfile and runtime requirements:

- source/deno and web/deno require capsule.lock.json and deno.lock or package-lock.json
- source/node and web/node require capsule.lock.json and package-lock.json
- python flows require uv.lock
- Tier2 flows require nacelle
- unsupported or out-of-policy behavior does not auto-fallback; it stops fail-closed

Current Deno executor rules include:

- deno.lock or package-lock.json fallback is required unless --no-lock is explicitly used
- provisioning performs deno cache first
- normal execution uses cached-only mode unless dangerous permission skipping is requested

## 7. Required Environment Checks

Before startup, ato validates required environment variables.

Supported declarations:

- targets.<label>.required_env = ["KEY1", "KEY2"]
- backward compatibility: targets.<label>.env.ATO_ORCH_REQUIRED_ENVS = "KEY1,KEY2"

Missing or empty required environment values stop execution.

## 8. GitHub Inference and Promotion

GitHub repository install and run flows rely on preview and normalization logic.

Current notable behavior:

- GitHub install drafts can return schema v0.3 style manifests
- preview TOML is normalized before installation
- legacy env.required is collapsed into required_env
- GitHub auto-fix can assign an available Ato-managed port to generated web manifests when port correction is requested
- runtime_version can be inferred for node, python, and deno draft installs
- inferred Deno apps must preserve run_command and execute via the dedicated Deno executor
- when deno.json references importMap, the referenced file must be included in pack.include

Debugging surface:

- --keep-failed-artifacts is a hidden flag on run and install for GitHub inference debugging

## 9. Native Delivery Specification

Native delivery is experimental.

Current product stance:

- primary user surface remains build, publish, and install
- `ato init` is the primary command for turning desktop source or imported artifacts into durable lock-first input
- `run` and `publish` should consume lock-derived desktop state rather than treat manifest authoring as the main contract boundary
- local finalize currently targets macOS darwin/arm64 with codesign

Current metadata policy:

- build always stages ato.delivery.toml into the artifact payload, even when only capsule.toml was authored
- install, finalize, and project read staged artifact metadata plus local-derivation.json
- the original source checkout is not required later for those flows

Current canonical lock contract policy for desktop native delivery:

- desktop-native semantics are surfaced under `contract.delivery`, not only through `contract.process`
- `contract.delivery.mode` is one of `source-draft`, `source-derivation`, or `artifact-import`
- `source-draft` means the project expresses native-delivery intent but build closure is still incomplete
- `source-derivation` means native-delivery intent plus build closure has resolved into `resolution.closure.kind = "build_closure"`
- `artifact-import` means an existing built artifact such as `.app` is being imported as compatibility input; this mode is provenance-limited and does not claim reproducible rebuild semantics
- `.app`, `.exe`, AppImage, and `.dmg` are never treated as canonical build inputs; they are build outputs or imported artifacts, so `contract.delivery.artifact.canonical_build_input` remains `false`
- `contract.delivery` is organized into `artifact`, `build`, `finalize`, `install`, and `projection` logical sections
- `local_derivation` and `projection` remain host-local execution/install metadata; they do not participate in canonical lock identity

Supported input matrix for the current roadmap:

| Input             | `init` | `run`  | `publish` | Notes                                                   |
| ----------------- | ------ | ------ | --------- | ------------------------------------------------------- |
| Tauri source      | target | target | target    | highest priority source-derived desktop path            |
| Electron source   | target | target | target    | second priority                                         |
| Wails source      | target | target | target    | third priority                                          |
| built `.app`      | target | target | target    | handled as `artifact-import`, not canonical build input |
| built `.AppImage` | target | target | target    | handled as `artifact-import`, not canonical build input |
| built `.exe`      | target | target | target    | handled as `artifact-import`, not canonical build input |

Command-first roadmap:

- Phase A: `ato init` learns to compile desktop source or imported artifacts into durable `ato.lock.json`
- Phase B: `ato run` and `ato publish` consume that lock-derived desktop state as lock-first consumers
- Phase C: compatibility manifest bridges and temporary manifest writes are pushed back into compatibility-only paths for build/publish; run/install may still retain transitional manifest-path surfaces during migration

Stable machine-readable contract fields for schema_version = "0.1" native JSON envelopes:

- fetch.json: schema_version, scoped_id, version, registry, parent_digest
- build JSON: build_strategy, schema_version, target, derived_from
- finalize JSON: schema_version, derived_app_path, provenance_path, parent_digest, derived_digest
- local-derivation.json: schema_version, parent_digest, derived_digest, framework, target, finalize_tool, finalized_at
- project JSON: schema_version, projection_id, metadata_path, projected_path, derived_app_path, parent_digest, derived_digest, state
- unproject JSON: schema_version, projection_id, metadata_path, projected_path, removed_projected_path, removed_metadata, state_before
- install JSON: install_kind, launchable, local_derivation, projection

## 10. Registry Behavior

### official registry

Current official registry examples include:

- https://api.ato.run
- https://staging.api.ato.run

Behavior:

- publish is CI-first
- local direct upload is not the normal path
- Publish only is the default phase selection

### Personal Dock

Behavior:

- default target when logged in and no registry is specified
- direct upload flow
- scoped_id can be auto-filled from handle and slug

### local and private registries

Behavior:

- direct uploads supported
- publish --artifact is the recommended flow
- --allow-existing is available only when the final Publish stage is included for private or local registries
- when registry serve --auth-token is enabled, publish requires ATO_TOKEN

Cross-device note:

- non-loopback exposure should use --auth-token
- install and run read APIs do not require the token
- publish does require the token when auth is enabled

## 11. Machine-Readable Output Contracts

## 11.1 Global JSON Error Envelope

When --json is enabled, ato can emit a structured error envelope with schema_version = "1".

Current envelope shape:

```json
{
  "schema_version": "1",
  "status": "error",
  "error": {
    "code": "E999",
    "name": "internal_error",
    "phase": "internal",
    "classification": "internal",
    "message": "...",
    "hint": null,
    "retryable": true,
    "interactive_resolution": false,
    "cleanup_status": "not_required",
    "cleanup_actions": [],
    "manifest_suggestion": null,
    "path": null,
    "field": null,
    "details": null,
    "causes": []
  }
}
```

Current diagnostic phase families:

- manifest
- inference
- provisioning
- execution
- internal

Current classification families:

- manifest for capsule.toml shape, missing fields, invalid phase inputs, and other declarative contract failures
- source for build scripts, source tree contents, compiler failures, and runtime entrypoint problems attributable to the user's code or assets
- provisioning for installation, fetch, staging, registry handoff, or environment preparation failures
- execution for process launch, readiness, supervisor, and runtime exit failures after admission into Execute
- internal for unexpected ato failures

Current cleanup fields:

- cleanup_status currently takes one of not_required, complete, or partial
- not_required means no compensating action was registered for the failed attempt
- complete means every registered cleanup action succeeded
- partial means at least one cleanup action failed while another cleanup action had already been attempted
- cleanup_actions is an ordered list of attempted cleanup operations such as remove_temp_dir, kill_child_process, or stop_sidecar
- manifest_suggestion carries a machine-readable fix proposal when ato determines the failure is a manifest problem with a concrete remediation
- interactive_resolution remains the switch for whether ato can offer or apply the suggestion interactively

Current diagnostic code families include:

- E001-E003 for manifest issues
- E101-E107 for inference and validation issues
- E201-E212 for provisioning, install, and managed publish payload limitation issues
- E301-E305 for execution issues
- E999 for internal errors

Current publish payload limitation contract:

- E212 means the managed Store publish path rejected or disallowed the current payload configuration
- E212 currently covers three cases: conservative preflight limit exceeded on the managed direct-upload path, large-payload override flags used on that path, or remote `413 Payload Too Large` returned by the managed upload path
- the current conservative preflight limit is 95MB and is a temporary fail-fast policy, not a remote acceptance guarantee

Current publish strategy contract:

- ato-cli now has a strategy boundary between direct upload and presigned upload
- the current default remains direct upload for all registries
- `ATO_PUBLISH_UPLOAD_STRATEGY=presigned` explicitly opts into the presigned strategy for compatible registries during P1
- the presigned strategy resolves or creates the capsule, starts a release, uploads the artifact to the presigned URL without `Authorization`, then finalizes the release through the registry API
- automatic capability-based selection is not enabled yet; host-based defaults remain in place until registry capability discovery and Dock parity are available

Current remediation contract:

- manifest-classified failures should include hint text whenever ato can point to a concrete field, declaration, or missing value
- when ato can safely suggest a declarative fix, manifest_suggestion should identify the target path or field plus the proposed edit
- source-classified failures must not be mislabeled as manifest failures; they can include hints, but ato should not offer automatic manifest edits unless the root cause is declarative
- the primary error code always describes the root failure; cleanup problems are attached as causes and do not replace the root error code

## 11.2 install JSON Output

Current install JSON includes these top-level fields:

- capsule_id
- scoped_id
- publisher
- slug
- version
- path
- content_hash
- install_kind
- launchable
- local_derivation
- projection
- promotion

`local_derivation` and `projection` are host-local derived artifact metadata. They may appear in install results and workspace-local files, but they do not participate in canonical lock identity or distribution lock content.

`PreparedRunContext.bridge_manifest`, execution plans, `config.json`, and native-delivery finalize/projection plans are derived artifacts. When an authoritative `ato.lock.json` is available, run/build flows must prefer lock-derived artifacts and must not rediscover manifest semantics from disk except inside compatibility-only paths.

Install kind currently includes:

- Standard
- NativeRequiresLocalDerivation

## 11.3 inspect requirements JSON

Current stable success shape includes:

- schemaVersion
- target
- requirements

Current requirements child keys include:

- secrets
- state
- env
- network
- services
- consent

## 12. Environment Variables

Current repository-documented environment variables include:

- CAPSULE_WATCH_DEBOUNCE_MS
- CAPSULE_ALLOW_UNSAFE
- ATO_TOKEN
- ATO_STORE_API_URL
- ATO_STORE_SITE_URL

Current defaults and roles:

- ATO_STORE_API_URL defaults to https://api.ato.run for search and install-related flows
- ATO_TOKEN is used for local/private registry publish auth and headless auth contexts
- CAPSULE_ALLOW_UNSAFE must be 1 to permit dangerous permission bypass

## 13. Filesystem and Host Paths

Current important paths:

- store install default output: ~/.ato/store/
- local registry default data directory: ~/.ato/local-registry
- engine registrations: ~/.ato/config.toml
- auth file: ${XDG_CONFIG_HOME:-~/.config}/ato/credentials.toml

Native delivery and projection on hosts currently use host-specific launcher surfaces:

- macOS: ~/Applications symlink projection
- Linux: .desktop launcher plus ~/.local/bin symlink projection

## 14. Current Compatibility and Deprecation Notes

Current compatibility and deprecation status:

- close is a stop alias
- pack is a build alias
- setup remains as hidden compatibility engine-install command
- --skip-verify is deprecated and rejected
- web target public is deprecated and rejected
- entrypoint = "ato-entry.ts" in web services mode is deprecated and rejected
- --legacy-full-publish is deprecated and scheduled for removal in the next major release
- source-project ato.delivery.toml is rejected; only artifact-internal ato.delivery.toml metadata remains

(Removed in recent versions: `SKILL.md` execution support.)

## 15. Recommended Authoring Patterns

### 15.1 Minimal source capsule

```toml
schema_version = "0.2"
name = "hello-deno"
version = "0.1.0"
type = "app"
default_target = "cli"

[targets.cli]
runtime = "source"
driver = "deno"
runtime_version = "1.46.3"
entrypoint = "main.ts"
required_env = ["API_KEY"]
```

### 15.2 Minimal web static capsule

```toml
schema_version = "0.2"
name = "hello-static"
version = "0.1.0"
type = "app"
default_target = "web"

[targets.web]
runtime = "web"
driver = "static"
entrypoint = "dist"
port = 3000
```

### 15.3 Dynamic app with services

```toml
schema_version = "0.2"
name = "my-dynamic-app"
version = "0.1.0"
type = "app"
default_target = "default"

[targets.default]
runtime = "web"
driver = "deno"
runtime_version = "1.46.3"
port = 4173

[services.main]
entrypoint = "node apps/dashboard/.next/standalone/server.js"
depends_on = ["api"]
readiness_probe = { http_get = "/health", port = "PORT" }

[services.api]
entrypoint = "python apps/control-plane/src/main.py"
env = { API_PORT = "8000" }
readiness_probe = { http_get = "/health", port = "API_PORT" }
```

## 16. Out of Scope for Stability Guarantees

The following are intentionally not treated as stable public contract unless otherwise documented:

- exact on-disk directory layouts under internal cache and native delivery working directories
- additive internal JSON fields beyond documented stable keys
- hidden command UX details
- experimental native delivery host and tool support beyond the documented current PoC

## 17. Source of Truth Policy for This Document

When this file and implementation diverge, implementation wins.

Priority order:

1. compiled CLI behavior
2. manifest validation and normalization code
3. install, publish, and runtime executor code paths
4. README examples and operational notes
5. this document
