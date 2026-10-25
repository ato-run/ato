# Formation 6b-B — workspace-scoped static build authoring

## State and exact pins

- Base: #1440 merged `a6033ecfb768f8aa00f08fd6cf489a25e0d04483`
  (reviewed head `476393f3bb37957b656dec80bc3410927d01f2ad`); #1438 `968836af`,
  #1439 `a3b9a5d1` unchanged.
- Execution / measurement pin **`c9500b37c91f6844f68b07cf1633d21144674dde`**;
  plan commit `56df7b20` ([plan](formation-node-static-workspace-plan.json),
  SHA256 `00bf84d711582da68e73189ea75911c8331ab4ccd7a7eb6174cfa70477894db5`).
  Later commits are rustfmt and ledger/docs only.
- Implemented, locally verified, actual fixture integration verified, actual
  six-app remeasurement verified. **Not merged at ledger creation. Not
  deployed. No remote migration. Model calls 0. #1421 untouched.**
- Contract: [node_static_workspace@1](../rfcs/draft/FORMATION_NODE_STATIC_WORKSPACE.md).
  Machine-readable: [ledger](formation-node-static-workspace.json).

## What was added

1. **Bounded Workspace Inventory** (`ato.formation-workspace-inventory/1`):
   root `package.json` `workspaces` only; literal or whole-segment `*`;
   ≤32 patterns, ≤4 segments, ≤4096 scanned entries, ≤64 workspaces, 1 MiB
   manifests; `..`/absolute/symlink crossings refuse; pnpm config deferred.
   Per-workspace typed qualification reusing the existing narrow static
   detector; root install scope reuses the exact v2 manager/lock/config rules.
2. **Opaque IDs**: `w_0…` over static-qualified workspaces only, private
   `NodeStaticWorkspaceAuthorization` (install scope + ID → cwd/output). The
   requester recomputes it from the digest-verified extraction.
3. **OperationCatalog**: `{"operation":"node_static_workspace@1","workspace_ids":[…]}`;
   invocation `{"operation":"node_static_workspace@1","workspace_id":"w_0"}`.
4. **Ato-owned canonical D**: install (`cwd .`, frozen/immutable,
   dependency-resolution requirement) → `<mgr> run build` (`cwd <workspace>`,
   denied) → browser serve (`root <workspace>/<out>`). Equal to the known-D v2
   builder's D for the same resolved scope (unit-tested).
5. Candidate-producer prompt v2 (v1 bytes/hash pinned). Proposal schema,
   one-round limit and Python operation bytes unchanged.

Known-D (CandidateProducer OFF) is unchanged: no automatic workspace route.

## W0–W12

| Case | Result | Where |
|---|---|---|
| W0 no workspace | `workspace_absent`; catalog has no workspace op | unit |
| W1 one static | one ID `w_0`; provider request has no path/manager/version | unit + F1 |
| W2 two static | `w_0`,`w_1`; known-D still `preset_node_static_v2_workspace` | unit |
| W3 unknown ID | `proposal_id_unauthorized` | unit + **actual F3** |
| W4 path/cwd/argv/output/network/runtime field | `proposal_schema` | unit |
| W5 escape | `..`/absolute/symlink → `workspace_path_escape`; `**` etc. unsupported | unit |
| W6 server-only | `workspace_server_dependency`, not in catalog | unit |
| W7 ambiguous output | unreadable/escaping outDir → `workspace_output_ambiguous`; compound build → `workspace_static_unproven` | unit |
| W8 manager/version conflict | workspace manager/Node/lock/config conflicts excluded; root lock/config refuses scope | unit |
| W9 network denied | D admitted by validator; admission `network_denied`; nothing ran | **actual F2** |
| W10 K mutation | K field → `proposal_schema`; non-template K cannot be frozen; compiled K == frozen K | unit |
| W11 compiler only | admitted D without Runtime/Verifier has no receipt (F2) | **actual F2** |
| W12 Runtime + Verifier | same-K **PASS**, `fully_satisfied = true` | **actual F1** |

## Fixed-producer actual acceptance (Linux aarch64, bwrap+landlock)

Fixture: Yarn 1.22.22 monorepo (`apps/web` static, `tools/vite` non-static);
archive `sha256:fc25688f…d5bdf3`. `tools/vite` is a fixture-local stand-in,
not Vite, so install/build need no registry. Path: inventory → authorization
→ FixedCandidateProducer (preregistered `w_0`) → production CandidateRegistry
→ canonical D `sha256:218c22fd…94967b0` → existing local Formation.

- **F1** (explicit local policy `dependency_resolution`): `yarn install
  --frozen-lockfile` at root, `yarn run build` in `apps/web`, serve
  `apps/web/dist`; verified route, ContractRef `sha256:7b5250ad…66bec50`
  (= frozen K), receipt `fully_satisfied: true` (root 200 + source identity).
- **F2** (denied): same D, `network_denied` at admission, no execution/receipt.
- **F3** (`w_9`): `proposal_id_unauthorized`, nothing compiled.

The proposal carried only an ID; the network grant in F1 is the local
Formation request policy, not a proposal field. Runtime Network proposal
searches remain denied-only; the ato-api receiver WASM is not rebuilt here.

## Six real apps: before/after

Same six archives (hashes checked before any run), network denied.

| App | A known-D (Producer OFF) | B inventory / fixed producer |
|---|---|---|
| Excalidraw | `preset_node_static_v2_workspace` (byte-identical to 6b-A) | 10 workspaces; install scope refused `preset_node_static_v2_manager_config` (root `.npmrc`); `excalidraw-app` = `workspace_static_unproven` (compound build, no plain preview); 0 IDs |
| Etherpad | same | `workspace_pnpm_deferred` |
| Gitea | same | `workspace_pnpm_deferred` |
| HedgeDoc | same | `workspace_pnpm_deferred` |
| Actual Budget | same | 14 workspaces; install scope refused (`.yarnrc.yml`); `packages/desktop-client` = `workspace_static_unproven`; 1 `workspace_server_dependency`; 0 IDs |
| JupyterLab | same | `workspace_pattern_unsupported` (`tests/test-*`) |

- Workspace candidates discovered: 24 (2 inventories), static-qualified 0.
- Operation published: 0; producer invoked: 0.
- Known-D reach delta **0**; proposal-authoring reach delta **0**;
  same-K verified delta **0**; functional success delta **0**.
- **Excalidraw**: workspace discovery succeeded, but the unchanged root manager
  configuration refusal applies and `excalidraw-app` does not meet the narrow
  static rule. Authoring reach **+0** (not the hoped +1); not converted to
  `network_denied`. No config interpreter or compound-script parser was added.
- **Actual Budget**: workspace discovery success → install scope
  `preset_node_static_v2_manager_config`, as preregistered.
- Etherpad/Gitea/HedgeDoc/JupyterLab: no standalone static candidate was
  produced; their frontend assets were not turned into static routes.

## Next blocker distribution (measured, not decided)

pnpm workspace configuration 3; root manager configuration 2 (both also have
no static-qualified workspace); workspace pattern grammar 1. Among discovered
workspaces the qualification gate is `workspace_static_unproven` 11,
`workspace_build_script_missing` 11, nested lock 1, server dependency 1.

## Tests

- macOS selected Rust (formation, formation-worker, runtime-attempt,
  receipt-authority): **721 PASS / 0 FAIL / 1 ignored** (702 + 19 new).
- Linux aarch64 same set at the execution pin: 717 PASS / 5 FAIL / 2 ignored.
  All 5 (`exec_projection_v1` ×3, `lowering_parity` ×1,
  `process_toolchains_v1` ×1) fail identically on base `a6033ecf`: the host
  already has Python 3.12.7 provisioned (`verify-provisioned-python` instead of
  `provision-python`). Not relabeled green; not changed here.
- Linux final head: new/affected suites 16 + 26 + 54 + 118 + 35 PASS.
- clippy `--all-targets -D warnings` PASS; rustfmt applied.
