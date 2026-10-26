# Hosted Formation of `ato.capsule/2` sources (Gate A)

> Implementation explanation with dated measurements. Language: en.
> This page does not adopt a specification or certify current deployment.

- Issue: ato#1570. Design spike: `.tmp/1570-20261026/DESIGN-SPIKE.md` (working notes).
- Branch: `feat/1570-hosted-formation-capsule-v2`, based on `dev` `287fe65ef`.
- Status: **Gate A implemented and measured locally on Linux.** Gate B (executing
  the formed bundle on a Hosted Runner) is a separate change.
- Non-goals: changing v1 semantics, the Runner privilege boundary, or the meaning
  of Frozen K / D / Receipt. No `ato pack` step is required of the uploader.

## 1. What a v1 hosted Formation does today

`apps/formation-worker/src/job.rs` fetches and verifies the source archive, parses
the authored manifest with `parse_capsule_toml` (`ato.capsule/1`), plans a
candidate (`plan_candidate`), builds/realizes it through the shared runtime
attempt, composes an `ato.formation-result.v2` and publishes it. The result
names a process-workspace or static-web materialization.

A `ato.capsule/2` source declares a portable application instead: workspace,
filesystem state, HTTP/scenario observations and one or more Derivations. It is
already the input `ato pack` compiles into an `ato.portable-application/1` wire
v3 bundle. Hosted Formation had no frontend for it.

## 2. Design

### 2.1 Schema routing (v1 is untouched)

`job.rs` reads only the declared `schema` (`ato_formation::capsule_toml::declared_schema`)
after it resolves the manifest (the control plane's accepted copy wins over the
tree's). A document declaring `ato.capsule/2` is handed to the new frontend; every
other document — including an unknown schema — keeps the v1 path byte-for-byte.
An unknown schema is therefore refused by the v1 parser as
`unsupported_capsule_schema`, as before.

### 2.2 The v2 frontend (`apps/formation-worker/src/v2_frontend.rs`)

1. `parse_capsule_toml_v2` — strict; its own `capsule_toml_v2_syntax` /
   `capsule_toml_v2_invalid` codes reach the reporter unchanged.
2. Materialize the control plane's copy as `capsule.toml` in the attempt's source
   root. `capsule.toml` is excluded from the bundle's workspace digest, so the
   source closure and the workspace tree identity do not move. A unit test pins
   this: rewriting only the manifest leaves the tree ref identical.
3. `ato_portable_application::build_authored_bundle_v2` — **the same builder
   `ato pack` calls**. No second compiler.
4. Prove the authored semantics reached the bundle: the `[[state]]`,
   `[[contract.observation]]` (HTTP), scenario observations, `[[binding]]` ids in
   the parse must equal what the bound Contract/Derivation carries, and each
   scenario must be a content-addressed `ato.contract.scenario@1` requirement
   with its canonical object. A mismatch fails closed as
   `capsule_v2_semantics_dropped` — a refactor that silently dropped `[[state]]`
   would otherwise still produce a valid bundle.
5. Derivation selection: one declared Derivation is the bundle's answer; more
   than one requires `authoring.derivation_ref` (`derivation_selection_required`),
   and a requested route that is not declared is `derivation_selection_not_declared`.
   There is no planner and no fallback.
6. `run_portable_bundle_attempt` (shared) — see §2.3.
7. A portable OCI route pulls its pinned image. That is only admitted under
   `policy.network = "dependency_resolution"`; otherwise the frontend refuses
   with `capsule_v2_network_not_admitted` before anything runs (fail closed).
8. The bundle is registered before the result names it
   (`PUT /v1/internal/formation/attempts/:attemptId/portable-bundle/:digest`,
   which answers `bundle_id`). A receipt that is not `fully_satisfied` is never
   published.

### 2.3 Shared portable attempt API

`apps/portable-application/src/portable_run.rs` (moved from
`apps/cli/src/portable_attempt.rs`, single implementation):

- `PortableBundleExecutor` makes a validated bundle's tree runnable and starts
  the selected Derivation (static, process, OCI container, OCI service group).
- `PortableRunContext { cache_root, sandbox_shim, sandbox_subcommand }` — the
  host-specific inputs that used to be CLI globals. The CLI passes `~/.ato/cache`
  and `__portable-sandbox-exec`; the worker passes its work-root cache and its
  `sandbox-exec`.
- `run_portable_bundle_attempt(request, permit)` — bundle bytes + the one selected
  Derivation + runtime scratch/cache/shim + network, profile, authorization →
  `run_reserved_attempt` (admission, start record, realization, HTTP/Scenario
  observation, `verify_observed_candidate`) → receipt, verification, realization
  evidence and outcomes.

**Why not `lib/runtime-attempt`:** `ato-portable-application` already depends on
`ato-runtime-attempt`, whose crate contract says the Runtime layer never depends
on an app. The bundle authority is `ato-portable-application`; both the CLI and
the Formation worker already depend on it. Placing the shared entry there is the
only direction that does not create a cycle.

The CLI's `start_and_verify_portable_application` keeps its behaviour: it imports
the moved executor and passes the same cache root and shim it always used.

## 3. Result wire (additive)

Protocol stays `ato.formation-result.v2`. A v2 result fills every key a v1 result
fills and adds:

| key | value |
|---|---|
| `formation_kind` | `"portable_v2"` |
| `schema_profile` | `"ato.portable-application/1"` |
| `portable_bundle_sha256` | the bundle file's transport SHA-256 |
| `portable_bundle_artifact_ref` | the control plane's `capsule_bundles.id` |

- `materializations[0]`: `kind = "portable_application"`,
  `media_type = "application/vnd.ato.portable-application.v3"`,
  `content_ref = digest = portable_bundle_sha256`.
- `realization_candidates[0]`: `kind = "portable_application"`,
  `derivation_ref`, `bundle_artifact_ref`, `schema_profile`.
- `contract_ref` = bundle root `ContractRef`; `derivation_ref` = the route that
  ran. `verification_receipt` is the existing
  `ato.contract-verification-receipt/1` unchanged.

`lib/ipc` gains only the `portable_application` variants of
`RequestedOutputV1` and `MaterializationKindV1`. v1 canonical bytes and golden
fixtures are unchanged.

## 4. Gate A measurements (Linux, local)

`ubuntu-sugamo`, branch HEAD measured below, `CARGO_TARGET_DIR=/data/cargo-target-1570`.
The API was the test fixture (claim/result/bundle-upload endpoints simulated, no
staging deployment); the bundle builder, Docker launch, HTTP observation and
verifier were real.

| case | command | result |
|---|---|---|
| memos OCI (opt-in) | `cargo test -p ato-formation-worker --test hosted_capsule_v2 -- --ignored --nocapture memos` | **PASS** (5.5s) |
| portable process | `cargo test -p ato-formation-worker --test hosted_capsule_v2` | **PASS** |
| unknown schema / invalid v2 / unadmitted OCI network / Node 20 policy | same file | **PASS** (typed refusals) |

Fixture conditions for these runs: the API is the local fixture (no staging
deployment); `policy.network = "dependency_resolution"` for the OCI case; the
formation host is Linux with Docker and the pinned image resolvable. The bundle
builder, the Docker launch, the HTTP observation and the verifier are real.

memos Gate A measured values (stable across runs):

```
contract_ref            = sha256:64368f746f00ed7be9c5e6846c99639d19d402bdc4e96b022407dc3f8863908b
derivation_ref          = sha256:ef094f174599784ad6e0d3dc70746bc365cd8ff6216a7356e98b9d6747ceda4e
portable_bundle_sha256  = sha256:5102e34d146f734a68ce8afc3e8a12debe1169cf73df113b7e256faa0d1e0962
verification_receipt.fully_satisfied = true
formation_kind          = portable_v2
```

portable process positive (same pipeline, no Docker):

```
contract_ref            = sha256:9bf1dde591e75171e28f258321116dff9a877d190d8ddad3da524c08b42227e5
derivation_ref          = sha256:769ff433b244677dd2c872a25d01fdf94e0d3d45f414f1c524c1a27d71591481
portable_bundle_sha256  = sha256:3bb9598aa897216cc6913d82de8dc9e74fc79953752174224483932da70f0b67
verification_receipt.fully_satisfied = true
```

(`portable_bundle_artifact_ref` is `bnd_fixture` in the fixture; in a deployed
stack it is the id the upload endpoint returns.)

**Why the DerivationRef is not an expected constant.** `ContractRef` is the
authority: it is the same `sha256:64368f74…` the local memos pack produces
because K is the authored Contract, independent of the workspace bytes.
`DerivationRef` also covers the bundle's workspace **tree**, so it differs
between an E2E fixture whose workspace carries different files and this test's
workspace (the sample's placeholder plus the fixture's own file). Gate A
asserts K, `fully_satisfied`, the receipt/ref self-consistency and
`formation_kind`; it does not require an expected D. A staging E2E should judge
the same fields and the bundle sha's self-consistency, not a pinned D.

## 5. Refusals

| input | code | receipt published |
|---|---|---|
| `ato.capsule/3` (unknown schema) | `unsupported_capsule_schema` (v1 parser) | no |
| malformed v2 (e.g. duplicate observation id) | `capsule_toml_v2_invalid` / `capsule_toml_v2_syntax` | no |
| authored declaration missing from the bundle | `capsule_v2_semantics_dropped` | no |
| several Derivations, no selection | `derivation_selection_required` | no |
| requested Derivation not declared | `derivation_selection_not_declared` | no |
| OCI route with `policy.network != dependency_resolution` | `capsule_v2_network_not_admitted` | no |
| Node release the runtime policy proves affected | `runtime_version_not_patched` | no |
| K not satisfied at runtime | the verifier's typed code (e.g. `scenario_step_failed`) | no |
| bundle registration failure | `capsule_v2_bundle_publication_failed` | no |

v1 regressions on the same host (final HEAD): `scenario_verification`,
`derivation_transformation`, `stateful_transformation`, `ato-ipc` (v1 golden
fixtures), `ato-portable-application --lib`, `ato-formation --lib`, and the
whole `ato-formation-worker` suite (172 lib tests) pass; `fmt`,
`cargo check --workspace --all-targets`, `clippy` (changed crates) and
`arch-check` pass.

Two failures seen in the wider earlier runs are **pre-existing/flaky**, not
caused by this change:

- `a_run_whose_contract_fails_keeps_nothing_running`
  (`apps/cli/tests/portable_application.rs:952:74`, `Os { code: 2, kind:
  NotFound }`) reproduces identically on unmodified `dev` `287fe65ef`
  (`/data/p1570-base/logs/01_cli_portable_base.log`): same test, same line,
  same error. Pre-existing on dev.
- `runtime_network::proposal::reasoning::autonomous_tests::
  operation_retries_are_initial_plus_three_and_survive_restart` is timing
  sensitive: it failed once in a full parallel run (assert `left: 3, right: 4`)
  and passed on serial rerun 3/3, on base `287fe65ef`, and in the final full
  run (172 passed / 0 failed).

The `hosted_capsule_v2` suite (including the process positive) passed **three
consecutive times**; it binds `127.0.0.1:0` for the API fixture and the
executor allocates a free loopback port per route, so it does not depend on a
fixed port.

## 6. Gate B (next change)

Hosted Run imports the registered `capsule_bundles` artifact and executes it
without a manual `ato pack`:

1. The control plane resolves `portable_bundle_artifact_ref` to verified bundle
   bytes and selects `derivation_ref` explicitly (never a planner).
2. The Hosted Runner imports the bundle through the existing capsule-bundle
   quarantine/validator path and starts it with its own lease.
3. Scenario verification runs through the existing Rust verifier; the receipt
   stays `ato.contract-verification-receipt/1`.
4. Acceptance: memos from source to running Hosted Run, then a scenario-bearing
   process application.

## 7. Remaining work / risks

- The result is consumed by the ato-api change
  `feat/1570-formation-v2-api`; both sides must keep the keys in §3 identical.
- Operator approval for `dependency_resolution` is an API/operator decision; the
  worker only enforces the policy it is given.
- Pulling the pinned OCI image is the formation host's Docker path, separate from
  the Run's network policy. No privilege was widened.
- Receipt validation for v2 results in the API, and staging deployment, are out
  of scope for this local Gate A.
- `stateful` scenario routes still fail closed as
  `scenario_isolation_unavailable` by design (v1 limitation, unchanged).

## 8. Verification commands

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets --locked
cargo clippy -p ato-formation-worker -p ato-portable-application -p ato-cli \
  --all-targets --locked -- -D warnings
cargo run --locked -p arch-check
cargo test -p ato-formation-worker --locked
cargo test -p ato-cli --test scenario_verification --locked -- --nocapture
cargo test -p ato-cli --test derivation_transformation --locked -- --nocapture
cargo test -p ato-cli --test stateful_transformation --locked -- --nocapture
# Gate A (opt-in: Linux Docker + the pinned memos image):
cargo test -p ato-formation-worker --test hosted_capsule_v2 --locked -- \
  --ignored --nocapture memos
```
