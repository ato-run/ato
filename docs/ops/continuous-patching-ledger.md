# Continuous patching ledger

Status: **record of what exists**, 2026-10-24. This is the first PR of the
continuous-patching work: it inventories the update paths that already exist,
names the gaps with evidence, and fixes the PR split. It does not claim that any
planned mechanism is available.

- `ato` base: `origin/main` `17305afe0` (2026-10-20)
- `ato-api` base: `origin/main` `4ed06682` (2026-10-24)
- Method: source reading and existing operational records only. No host access,
  no production/staging queries, no deployment. Statements about running
  binaries or environment configuration are explicitly marked unverified (§7).

Paths are relative to each repository root; `ato-api:` marks the other repo.

## 1. Scope, and the three things that must not be conflated

The goal is to turn CVE response from an incident into a repeatable operation:
detect, determine impact, build a candidate, re-verify, switch, and keep the
evidence. The design constraint is that this must not break Capsule identity.

| Object | What is updated | Who owns the decision | Compatibility check |
|---|---|---|---|
| L0 Control plane (`ato-api`) | Worker code, routes, authz, broker | Ato (operator) | API contract + tests; migrations forward-only |
| L1 Worker / Runner | `ato-formation-worker`, `ato-connected-realization-worker` binaries | Ato for `ato_managed`; device owner for `user_managed` | heartbeat/capability contract, drain, lease/attempt fencing |
| L2 Runtime | OCI images, OS, Node/Python, base image digests | Ato for platform-managed images; author for user-pinned images | the same Contract K, re-executed |
| L3 Application / OSS source | The app's own source, npm/pip/cargo dependencies | Author (developer intent) | new Contract; author approval |

L1/L2 are platform updates: the application source does not change. L3 changes
what the application *is*, so it must keep its own approval and verification
policy. A runtime patch that changes a Derivation but satisfies the same K is
still the same Capsule; a patch that forces a different K is a different K and
must never be recorded as the old one (§5).

## 2. Layer-by-layer current update path

### L0 — Control plane

- Deploy is explicit-environment only: `pnpm deploy:staging` /
  `pnpm deploy:production` → `ato-api:scripts/deploy-worker-safe.mjs`, which
  refuses bare `wrangler deploy`, refuses missing secrets, checks the active
  version and pins the Wrangler version (`ato-api:package.json:10-13`,
  `ato-api:scripts/deploy-worker-safe.mjs:8-13,60-130`).
- D1 migrations are **not** applied by CI. They are applied by a human per
  environment, forward-only, and a code change that reads a new column must not
  merge before that environment is migrated (`ato-api:.github/workflows/deploy.yml:12-18`).
  Latest migration on the base commit: `0353` (`0350`/`0351` are absent by
  staging-first history; the ledger is keyed by complete filename).
- Rollback is a redeploy of the previous Worker version. Migrations are not
  rolled back; the operational rule is forward-fix, not schema rewind.

### L1 — Worker / Runner

**Connected realization worker (`ato_managed` fleet).**

- Artifacts are content-addressed in R2:
  `infra/runner-artifacts/<platform>/<sha256>/runner`
  (`ato-api:scripts/publish-runner-artifact.mjs:37-60`). Existing objects are
  not overwritten.
- Fleet bootstrap downloads the artifact and verifies **size and sha256** before
  installing it and enabling the unit (`ato-api:src/services/managed_cpu_fleet.ts:824,834`).
- The existing physical-host CPU guests are updated one at a time by
  `ato-api:scripts/ops/existing-host-cpu/upgrade-oci.py` behind an expected
  digest and a fresh drain receipt (`:17,64-72`).
- Other hosts: manual procedure in `docs/ops/runner-rollout-and-rollback.md`
  (drain → confirm Runs stopped → swap binary / systemd drop-in → verify
  recovery → undrain).

**Formation worker.**

- Built by CI for test purposes only. Both workers set
  `[package.metadata.dist] dist = false`; `dist-workspace.toml:35` publishes
  `ato-cli` only. There is no published worker artifact, checksum or signature
  in this repo.
- Identity is stamped at build time by `apps/formation-worker/build.rs:22-86`
  and printed by `--version` and as the first `serve` log line
  (`apps/formation-worker/src/bin/ato-formation-worker.rs:23-42`). Undeterminable
  fields print `unknown` instead of a plausible value. The operational rule is
  to record the binary's SHA-256 and the `--version` line together
  (`docs/ops/formation-worker-build-identity.md:43-64`).
- Deployment is manual: user systemd units on `ubuntu-sugamo` with
  `ExecStart` paths into `~/ato-formation-candidates/...`; the unit files and
  deploy script are **not in the repository**
  (`ato-api:docs/ops/task-a-formation-diagnostics-staging-2026-09-11.md:56-85`,
  `ato-api:docs/ops/release-jsx-storage-plaza-2026-09-12.md:59-79`).

**The update directive that exists, and has no consumer.**

`POST /v1/runners/:id/update` records `update_requested_version`; the heartbeat
returns `update: { minimum_version }` only for `user_managed` devices
(`ato-api:src/routes/runners.ts:700-790,1443-1452`). The comment at
`:706-712` says the runner "self-updates to latest + re-execs on its next
heartbeat". **That consumer does not exist in this repository**: the connected
worker's heartbeat discards the response body
(`apps/connected-realization-worker/src/lib.rs:4501-4548`), and the Formation
worker has no heartbeat at all. This is a
[Detected mismatch](https://github.com/ato-run/ato) between the comment and the
code, and it is the main reason L1 cannot yet be described as continuously
patchable.

### L2 — Runtime images

- Hosted OCI execution is digest-pinned by the author/Derivation, not by the
  platform: `ato:lib/formation/src/capsule_toml_v2.rs` requires an exact digest
  for an OCI route, and `ato:lib/formation/src/execution.rs` verifies it.
  A patched base image is therefore an author-side image rebuild unless the
  platform owns the image.
- Formation's own source→OCI path freezes a base image by digest and builds
  offline (`docs/ops/formation-source-oci-d2.md`). The record there is
  "implemented · unit-tested · actual offline integration verified" on one
  host, **not merged/deployed** as of its own writing.
- Node is a platform-managed toolchain/runtime catalog, not a floating
  dependency: `lib/formation/src/intent.rs:482` (`NODE_VERSION_LADDER`),
  `:486` (`DEFAULT_NODE`), `:1028` (`SUPPORTED_NODE_RUNTIME`), and
  `lib/formation/src/preset.rs:67` (`SINGLE_JSX_NODE_VERSION`). The single-JSX
  preset also packages `SINGLE_JSX_REACT_VERSION` (`:70`).
- There is **no** platform-managed image inventory, SBOM, vulnerability scan,
  image signature/attestation or provenance record for the images Ato runs.
  Concretely: no image-digest-keyed table exists; `build_attestations` has no
  producer; `attestation_status`/`provenance_status` are assigned without
  verification; the Ed25519 bundle signature primitive
  (`lib/objects/src/bundle.rs:433`, verify `:478`) has no producer path; CI
  publication verifies a DID signature + GitHub OIDC for published artifacts
  (`ato-api:src/routes/publish.ts:506-540`) but that is not an image attestation.
  There is also no `cargo-audit`/`cargo-deny`/Dependabot/Renovate configuration
  in either repository. What exists is (a) the manual "sha256 + `--version`"
  deployment discipline above, (b) size/sha256 verification of runner
  artifacts, and (c) `npm audit` recorded in some receipts for imported
  third-party code.
- Contract verification already has a single authority:
  `ContractVerificationReceipt` carries `execution.image`, container/attempt
  identity, observations and `fully_satisfied`
  (`lib/formation/src/verify.rs:298`); `accept_receipt` /
  `accept_verified_route` decide acceptance (`lib/formation/src/receipt.rs:63,277`),
  and `ato-api` calls the same Rust authority through WASM
  (`ato-api:src/services/runtime_network/receipt_authority.ts:252,285`).
  Receipts are stored inside `formation_results.result_json.verification_receipt`,
  `formation_attempt_reports.report_json`, and
  `verified_routes.verifier_receipts_json` — there is **no receipt-by-image
  index**, so "which image was last verified, under which K and D" is not a
  query today.
- The rebuild path already exists: source patch →
  `createSourceSchema` → `submitFormationJob` → `acceptResult`
  (`ato-api:src/services/compute_sources/service.ts:59,367`,
  `ato-api:src/routes/compute_sources.ts:276`). Re-verification after a rebuild
  needs the existing authority functions, not a new K implementation.
- Consequently, a runtime patch today is: author rebuilds/repins the image or
  operator changes the Node catalog → Formation produces a new Derivation →
  Contract K is re-run through the same authority → the receipt is stored, but
  not indexed by image. **There is no automated path from "advisory" to
  "candidate" to "verified switch".** Phase 2 (PR4) starts by making the
  inventory and the patch record real: a versioned ledger file with a validator
  (PR4), then the D1 image/receipt index, then the candidate pipeline.

### L3 — Application / OSS source

- Source revision updates already flow through the existing
  `/computes/:id/update-from-source` path, producing a new Derivation; the
  Contract verification receipt is produced by the Rust authority
  (`tools/receipt-authority`, `apps/formation-worker` attempt verification).
- Updating the OSS source itself changes what the application is. It must keep a
  distinct approval/verification policy (author intent, license/behaviour
  compatibility), and it is out of scope for the Worker/Runtime patch path.

## 3. Formation worker lifecycle: exactly what is implemented

| Stage | Current implementation | Evidence |
|---|---|---|
| Start | `serve --api-base --work-root [--worker-id] [--poll-ms]`, token in `ATO_FORMATION_TOKEN`; refuses to serve when containment is unavailable | `apps/formation-worker/src/bin/ato-formation-worker.rs:73-97` |
| Claim | `POST /v1/internal/formation/claim?worker_id=…`, bearer token; one job at a time, synchronous loop | `apps/formation-worker/src/api.rs:105-122`, `bin/…:116-220` |
| Concurrency | 1 job; Runtime Network lane advertises `capacity: 1` and one ticket at a time | `apps/formation-worker/src/runtime_network.rs:2073-2077,2104-2157` |
| Signals | none (`SIGTERM`/`SIGINT`/`signal_hook`/`ctrlc` absent in the crate) | rg in `apps/formation-worker/src` |
| Drain | `drained_at` exists and is honored by connected-runner selection, lease claim, Runtime Network selection/claim, and managed CPU fleet — **not** by the Formation claim | `ato-api:src/routes/internal_formation.ts:211-262` vs `ato-api:src/routes/runtime_network.ts:189-192` |
| Crash | killed worker leaves the attempt `running` in D1; the claim query only takes `status='queued'`, so the polling path never picks that job up again | `ato-api:src/routes/internal_formation.ts:225-231`, `ato-api:src/services/formation/registry.ts:99-104` |
| State report | none; the Formation worker has no heartbeat, version report, or lifecycle state | — |
| Rollback | manual only: `docs/ops/runner-rollout-and-rollback.md` (binary swap via systemd drop-in) | `docs/ops/runner-rollout-and-rollback.md:38-106` |
| Runtime Network drain | the coordinator excludes a drained Runtime and issues no ticket; but the worker itself has no signal/drain path and learns nothing after the fact | `ato-api:src/services/runtime_network/select.ts:131`, `ato-api:src/services/runtime_network/coordinator.ts:1250`, `ato:runtime_network.rs:1998-2162` |

Reusable pieces (do not build a parallel subsystem):

- `runner_provisions` is already a crash-recoverable, cron-driven step machine
  with `drain_requested_at`, `save_deadline_at`, `drain_lease_ids_json` and a
  per-minute tick (`ato-api:src/db/schema.ts:10488-10540`,
  `ato-api:src/services/runner_provisions.ts:1217,1338`,
  `ato-api:src/index.ts:1156-1171`). Its vocabulary
  (`requested → provisioning → waiting_runner → active → … → terminating`) is the
  closest existing shape for a worker update lifecycle.
- `AttemptJournal` is an atomic JSON journal implementation that treats a killed
  process as UNKNOWN rather than as success (`lib/runtime-attempt/src/journal.rs:199-300,373-430`).
- `admin_audit_events` records operator actions before/after with a request id
  (`ato-api:src/services/admin_operations.ts:32-64`).
- Admin drain/undrain (`POST /v1/admin/runners/:id/drain|undrain`) already
  exists and is the correct operator primitive to build the update flow on.

## 4. First validation case: CVE-2026-111497 / RUSTSEC-2026-0348

PR3 adds the impact determination as
`docs/ops/cve-2026-111497-node-brotli-20261024.md`. This ledger keeps only the
conclusion, because "version matches" is not an impact determination:

| Component | Version in use | Brotli reachable? | Conclusion |
|---|---|---|---|
| Formation build sandbox | Node ladder `18.20.4/20.20.2/22.14.0/24.18.0` | only inside already-untrusted build code; bwrap+Landlock required | version affected; boundary-crossing RCE **not demonstrated** |
| Formation runtime (`[[runtime]]`) | `20.20.2`, `22.14.0` | depends on the app's own HTTP stack | **undetermined per app**; catalog updated in PR3 |
| Hosted OCI app image | author-pinned digest | inside the author's image | platform not affected; per-app review |
| `ato-api` (workerd) | — | no Brotli decode in repository code | not affected in repo-visible code |
| Rust workers / CLI (rustls) | `rustls 0.23.40`, `0.21.12` | non-default `brotli` feature is **off**; no `brotli` crate in any `Cargo.lock` | **not affected**; do not change |

Policy carried forward: a CVE is not a version string. The determination records
the feature/configuration and the trust boundary, and the runtime catalog update
is defense-in-depth plus conformance to Node's security line — not proof that an
exploit was possible.

## 5. Capsule identity rules for a runtime patch

- Capsule is immutable and addressable. A runtime patch never rewrites a
  Capsule, a ContractRef, or an existing receipt.
- Derivation and execution image are Materialization. Rebuilding with a patched
  runtime yields a **new Derivation** and a new image digest. If that
  Derivation satisfies the same K, the Capsule stays the same and the new
  receipt is recorded next to the old one.
- If K itself pins the runtime or an execution observable changes, the result is
  a different K → different Capsule. Record it as such; do not claim continuity.
- Never delete old derivations, image digests, or receipts. The update history
  is the evidence.
- Run acceptance requires all observations `Satisfied`; `Deferred` is not enough
  (workspace `AGENTS.md`, portable application section).

## 6. PR split and status

| PR | Repository | Branch | Scope | Status |
|---|---|---|---|---|
| 1 | `ato` | `docs/continuous-patching-ledger` | This ledger | [#1546](https://github.com/ato-run/ato/pull/1546) |
| 2a | `ato-api` | `feat/formation-worker-update-control` | Formation claim honors drain; managed worker state report; operator update request; additive migration | [#879](https://github.com/ato-run/ato-api/pull/879) |
| 2b | `ato` | `feat/formation-worker-update-lifecycle` | Worker-side lifecycle journal, drain/stop, version+digest verification, operator runbook | [#1547](https://github.com/ato-run/ato/pull/1547) |
| 3 | `ato` | `fix/cve-2026-111497-node-brotli` | Node catalog moved off affected/EOL versions; CVE receipt | [#1548](https://github.com/ato-run/ato/pull/1548) |
| 4 | `ato` | `feat/runtime-image-ledger` | Versioned runtime-image ledger (`ato.runtime-image-inventory/1`: digest, base digests, runtime versions, declared SBOM/scan/provenance, Derivation, Contract K + receipt refs) and a fail-closed patch-candidate validator. It records declared evidence; it does not scan, sign, or produce receipts. | [#1549](https://github.com/ato-run/ato/pull/1549) |
| 5 | TBD | — | D1 index: one `runtime_images` row per digest (base digests, runtime versions, SBOM/scan artifact refs, Derivation, latest receipt), written at Formation acceptance, read-only list route. Reuses the existing receipt schema and authority; no new K evaluator in TypeScript. | planned |
| 6 | TBD | — | Patch candidate rebuild + Contract K re-verification harness on the existing `source-patch → submitFormationJob → acceptResult` path, emitting a receipt and a ledger record. | planned |
| 7 | TBD | — | Staging auto-update E2E (needs host access; cannot be verified from a source checkout). | planned |
| 8 | TBD | — | Advisory → update candidate generation (Auto-build first; Auto-rollout only after the safety conditions are measured). | planned |

This is the recommended review/merge order, and it is slightly finer than the
original six-PR outline: the worker update is split per repository (2a/2b), the
CVE fix is its own PR (3), and the runtime work is split into inventory (4), the
D1 index (5) and rebuild/re-verify (6). The only hard dependency inside the
split is 2a → 2b (they share a wire contract); 3 and 4 are independent of the
worker update and of each other; 5 depends on 4; 6 depends on 5; 7 and 8 need
the evidence from the preceding PRs.

PRs are deliberately independent. 2a and 2b share a wire contract but land
separately: 2a is safe with no worker change (a drained worker simply stops
claiming once the old binary is replaced), and 2b degrades to "no directive"
against an older API.

## 7. Verified vs not verified

Verified from source (with the anchors above):

- All L0-L3 update paths and the exact absence of a Formation drain/state path.
- The update directive's condition and the absence of a Rust consumer.
- No SBOM/signature/scan/audit infrastructure in either repository.
- The manual binary-swap runbook and the build-identity discipline.

Not verified here (requires host/Cloudflare/D1 access):

1. The actual systemd unit files and `ExecStart` paths on `ubuntu-sugamo`.
2. The SHA-256 and stamped identity of the currently running workers, and
   whether they were built from `main`.
3. Whether `RUNNER_MIN_AGENT_VERSION` is configured in staging/production.
4. `drained_at` / `update_requested_version` / `agent_version` values in the
   real `runner_devices` rows, and the count of `formation_jobs` stuck in
   `running`.
5. Whether any manual artifact publication happens outside CI.
6. Host containment (`bwrap`/Landlock/Docker) state.

## 8. Conservative operating rule until Phase 2 lands

- Do not patch a running container in place, and do not kill an in-flight build
  to make an update look fast.
- For a critical advisory: drain new Formation claims at the control plane,
  record the running binary's SHA-256 and `--version`, let in-flight attempts
  finish, then swap the binary and re-run Contract K. If a worker cannot be
  trusted, stop new claims and stop assigning work; do not report the old
  runtime as safe.
- Removing a runtime version from the catalog stops **new** resolution; it does
  not retroactively stop a sealed Derivation that already pinned it. Such a
  Derivation keeps its pin and fails closed only when the toolchain is absent;
  if the old toolchain is still on the host it can still run on the affected
  runtime. Until the rebuild path (PR6) replaces it, report those runs as not
  patched — a catalog change is not evidence that they are.
- Never report "no known vulnerabilities" on the basis of a scan that does not
  exist. Report the last check date, the known advisories, the patch status and
  the verification result (or the absence of one).

## 9. Progress since this ledger (2026-10-24, second batch)

The first batch (this document's PRs 1–4) and the second batch below are
deliberately separate. Nothing here changes the §7 rule: a mechanism existing is
not the same as a host, staging, or production having exercised it.

| PR | Repository | What it adds | State |
|---|---|---|---|
| [#1550](https://github.com/ato-run/ato/pull/1550) | `ato` | Fix the pre-existing base-main Linux build break (`OciSpec.user` in the source OCI runtime) so CI evaluates real diffs | **merged** |
| [#1551](https://github.com/ato-run/ato/pull/1551) | `ato` | Execution-time runtime policy: refuse a sealed Derivation pinned to an affected Node (`runtime_version_not_patched` / `_unverifiable`) at attempt admission, bundle validation and retained replay; one authoritative table shared with the authoring catalog. Stacked on #1548. | open |
| [#880](https://github.com/ato-run/ato-api/pull/880) | `ato-api` | Hosted-start mirror gate for stored runtimes / `runtime_requirements` / `public_env.PATH`; persists `launch_blocked_reason`. | open |
| [#881](https://github.com/ato-run/ato-api/pull/881) | `ato-api` | `runtime_image_index`: verified receipt → image digest → D/K → latest receipt, written from Formation acceptance (no rows today) and the hosted-run ack (real rows). Not a K evaluator, not a scanner. | open |
| [#882](https://github.com/ato-run/ato-api/pull/882) | `ato-api` | Derivation re-formation orchestration: same source archive re-formed, old/new `contract_ref` compared, `k_equal` recorded; `accepted` only when `k_equal && fully_satisfied`. Archive lane only; no expiry/approval bypass. | open |

What is still **not** proven, and must not be reported as done:

- A real host/systemd drain → swap → verify → undrain E2E (PR7 in §6).
- A real host re-Formation of an existing app with a patched worker, with old D
  ≠ new D, old K = new K and a `fully_satisfied` receipt.
- Real image rows in `runtime_image_index` from an actual `.capsule` ack, and the
  D1 migration application in staging/production.
- Any "patched" claim derived from a receipt: the node runtime version is still
  not projected into `ContractVerificationReceipt.execution`, so the refusal
  code plus the declared Derivation remain the only evidence. Do not render
  "Patched" from a successful run.
- Auto-build from advisories and Auto-rollout (PR8 in §6) are not started.
