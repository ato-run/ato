# Formation 6b-A — canonical Node build route and eight-app remeasurement

## State and exact pins

- Docs correspondence #1439: head `d626202de2320779de9538cbf6c2044bf8654d0a`,
  merged `a3b9a5d14696514bfbbb20b6e80d0308e4b20d42`.
  5a = original Phase 3; 5b/within-search bounded 5c close Phase 4;
  independent 6a measurement, 6b capability expansion; **6c = original Phase 5**
  external-trigger, authorized known-D-first background revalidation.
- Qualification #1438: head `dbd1469adbc5cd9d58b6fbdf53c512f430a42dca`,
  merged `968836af358615844838fb7543a3832d52119808`. Eight pins unchanged.
- Implementation PR [#1440](https://github.com/ato-run/ato/pull/1440), base
  `968836af358615844838fb7543a3832d52119808`.
- First implementation `28d7e2f9cbac687f04fecf130c2002b2cd730db1`;
  actual clean Linux build/test/measurement pin
  **`177b61489e1fb28539868f9d9a76ca21fc6d61ca`**.
- Controller/plan commit `9f9b8d7a5613e56f90a8ac2907b4ec6fa25200f5`;
  plan SHA256 `c788cdb08ac4c64599e4a1cc019415176f4cc8d7ce162bfdb1bf5448ad22803d`.
  Subsequent ledger/docs commits do not change execution/harness bytes.
- Implemented, locally verified, **actual eight-app remeasurement verified**.
  At ledger creation PR unmerged; merge status remains a separate GitHub action.
  **Not deployed. No remote migration, no model calls, #1421 untouched.**

## Canonical route / authority

[Versioned contract](../rfcs/draft/FORMATION_NODE_STATIC_V2.md).
`node-static/v1` remains npm; old canonical K/D and execution-plan goldens pass.
The separate `node-static/v2` known-D frontend emits exact Node + pnpm/Yarn
requirements and explicit typed `process exec install → exec build → browser
serve`. Install uses frozen-lockfile (pnpm/Yarn 1) or immutable (Yarn >=2).
Cwd `.` and output root are canonical; dependency network is a requirement,
not permission. D contains no concrete host/toolchain path or Runtime ID.

Reuse: existing NodeEvidence lock detection, narrow static-artifact detector,
`resolve_toolchains` / ResolvedPackageManager and exact Node/manager provisioner.
New evidence facts: devEngines.packageManager and workspace presence. Primary
packageManager wins; contradictions, unpinned managers, lock conflicts and Bun
are refused. Physical execution binds a manager to its exact provisioned path,
not a host PATH fallback. No Corepack version authority or second resolver.

No arbitrary workspace inference. Manager redirect/hook configuration is also
conservatively refused in this slice. This is a limitation, not proof that every
such source is unsafe. Source scripts remain untrusted within existing sandbox
admission. Compiler success never establishes K; only Verifier can do so.
Capsule canonical representation/identity is outside this change.

## Tests

- T0–T21 **PASS**, plus four compatibility/negative tests: manager config/output escapes,
  npm lock + foreign manager conflict, legacy source-only manager inference and
  explicitly authored PATH (26 tests).
- Selected Rust regression: **702 PASS / 0 FAIL / 1 existing ignored**.
  formation, formation-worker, runtime-attempt, receipt-authority; includes 5a,
  fixed/general producer semantics, request compatibility and restart/fences.
- Isolated Linux aarch64: 26 v2 tests + 19 local Formation tests = **45 PASS**.
  New local test executes the normal source/freeze/plan/admission path for pnpm,
  Yarn 1, Yarn >=2: network denied, no workload execution or receipt. macOS
  correctly refuses earlier at unavailable containment; no admission changed.
- T17/T18 prove exact provisioning plans and absolute binary binding. They are
  **not** a fresh package download/install or successful v2 build assertion.
- Clippy selected crates/all-targets, fmt and diff check **PASS**.

## Actual before/after: unchanged eight archives

All eight went through the existing Rust `coverage_baseline` → `local::run`
path on isolated Linux with a fresh target directory and clean pinned source.
All archive hashes were checked before the first invocation. Producer and
DecisionProvider OFF, network denied, max attempts 4, no app-specific D.
Full repo/ref/archive hashes, raw observations and binary hashes are in
[JSON ledger](formation-node-build-8.json); [plan](formation-node-build-8-plan.json).

Every old terminal was `preset_node_static_needs_lockfile`:

| App | New terminal |
|---|---|
| Excalidraw | `preset_node_static_v2_workspace` |
| SearXNG | `preset_node_static_needs_lockfile` (no root lock) |
| Etherpad | `preset_node_static_v2_workspace` |
| Gitea | `preset_node_static_v2_workspace` (pnpm workspace configuration presence) |
| HedgeDoc | `preset_node_static_v2_workspace` |
| Actual Budget | `preset_node_static_v2_workspace` |
| JupyterLab | `preset_node_static_v2_workspace` |
| Grist core | `preset_node_static_v2_manager_config` |

**8/8 typed terminal classifications; 0 successful applications.**
The six workspace refusals include configuration-only and backend workspaces;
that count is not a count of six static-buildable applications.

- Primary taxonomy: known-D/authoring **8** (unchanged).
- Detailed distribution: workspace/config authoring **6**, manager config **1**,
  root lock absent **1**. Seven diagnoses refined, not seven authoring successes.
- **Authoring reach delta 0** (0 → 0).
- **Same-K verified coverage delta 0**.
- **Functional application success delta 0**.

All eight stop before K/D binding: no ContractRef, DerivationRef, execution
attempt ID, Verifier receipt or retained result is fabricated. No install/build
was executed for these eight. This remeasurement does not retest the other 12;
the historical 20-app ledger still records one typed-K receipt and zero
established functional applications, not an increment in coverage.

## Next measured generic gap (not implemented here)

Workspace-scoped static build authoring is the largest observed group. Qualify
an explicit workspace/build-target/output contract next, with Excalidraw and
Actual Budget as two potential frontend consumers. Do not guess monorepo routes
or turn Etherpad/Gitea/HedgeDoc/JupyterLab backend assets into static apps.
Manager-config interpretation is a separate one-app diagnostic; 50/100 apps,
multi-service generalization and 6c remain outside this PR.

## CI classification, not a green claim

Base Rust CI [36523322247](https://github.com/ato-run/ato/actions/runs/36523322247)
at exact base above and head
[36524301044](https://github.com/ato-run/ato/actions/runs/36524301044)
at `61c8d68f5ce01159a57007ea5fff3d1e870d6a56` are red.
Matched base failures: Windows browser_sandbox Unix APIs; Ubuntu hosted Python/
Node integration; macOS portable process ownership and five hosted-validator
candidate-never-listened failures. CLI Ubuntu passed; macOS/Windows red.

Two additional observations **are not baseline-reproduced failures**:
- macOS Activity MCP `WouldBlock`, mock request read at activity_mcp.rs:191.
  Source and call path unchanged; local isolated repetition **3/3 PASS**.
- Ubuntu portable static server unauthorized-state POST response read
  `ConnectionReset` at portable-application/src/lib.rs:3547. Base passed. This
  self-contained existing bundle/server test does not use Node v2 authoring.
  Isolated Linux repetition **3/3 PASS**; not baseline-reproduced.

These are recorded separately from the new route regression tests and must not
be relabeled baseline or silently made green. No unrelated test/code was fixed.

## Final compatibility review and remeasurement

The physical binding initially also rewrote legacy source-only manager argv.
Before final review it was narrowed to **explicit exact D runtime requirements
and default (not authored PATH) binding**. New v2 defaults still use only the
exact provisioned binary. Two regressions protect historical authored semantics.

The initial eight-app ledger at `b5b3dcaef1cf3c13d40ab6722299ebb149563b6b`
remains in history. A new clean build at `177b61489e1fb28539868f9d9a76ca21fc6d61ca`
remeasured the same eight pins with a fresh scratch root; all eight raw
observations are byte-identical to the first measurement. Source/ref/hash/K
policy was not tuned to outcomes. The final plan was repinned before this run.
The completed CI comparison above predates this narrow compatibility correction;
final-source local/Linux tests are separate, not a claim of final-head green CI.
