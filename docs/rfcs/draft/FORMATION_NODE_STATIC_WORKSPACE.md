# node_static_workspace@1 — workspace-scoped static build authoring

Status: 6b-B implementation contract; no deployment, remote migration or
live model call. Extends [node-static/v2](FORMATION_NODE_STATIC_V2.md) without
changing its non-workspace K/D bytes or its known-D refusal.

This is not monorepo inference. Ato reports a bounded set of workspace facts;
an owner-authorized typed operation may select one of Ato's opaque workspace
IDs; Ato alone resolves the ID and compiles canonical D. `ato.capsule/1`
grammar is unchanged: `[[runtime]]`, `[[derive.step]]` `cwd`/`argv`/`source`/
`root`/`network` already express the route.

## Workspace Inventory (facts, not selection)

`ato.formation-workspace-inventory/1`, read from the frozen source closure:

- Only the root `package.json` `workspaces`: a string array or
  `{ "packages": [...] }` (any other key is `workspace_declaration_unsupported`).
  `pnpm-workspace.yaml` is `workspace_pnpm_deferred` (separate slice).
- Pattern grammar: literal segments (`[A-Za-z0-9._@-]`, not `.`/`..`) or a
  whole-segment `*`. `**`, partial globs, negation, braces, `.` are
  `workspace_pattern_unsupported`. Absolute paths, `..` and any symlink crossed
  during expansion are `workspace_path_escape` for the whole inventory.
- Bounds: ≤32 patterns, ≤4 segments, ≤4096 scanned directory entries,
  ≤64 matched workspaces (`workspace_inventory_bounds`), 1 MiB manifests.
  `*` skips dot-directories and `node_modules`; a match needs `package.json`.
- No network, no package script, no source evaluation, no symlink followed.

Root installation scope (`installation`): `Exact { node_version,
package_manager, package_manager_version, install_mode }` or `Refused { code }`,
using the unchanged v2 rules: Bun deferred, root manager configuration refused
(`.npmrc`, `.yarnrc*`, `.pnpmfile.*` — no config interpreter), exact
`packageManager`/`devEngines.packageManager`, one matching lockfile.

Per workspace (`relative_root`, optional `package_name`, `qualification`):
`static_qualified { output_root }` or `excluded { code }`. Exclusions:
`workspace_symlink_input`, `workspace_package_json_{invalid,bounds}`,
`workspace_nested_lock`, `workspace_manager_config`,
`workspace_nested_workspaces`, `workspace_manager_conflict`,
`workspace_node_version_conflict`, `workspace_build_script_missing`,
`workspace_server_dependency`, `workspace_output_ambiguous`,
`workspace_static_unproven`, `workspace_entry_missing`,
`workspace_path_unsupported`. Static qualification reuses the existing narrow
detector (plain `vite build` + `vite preview`, no known server framework,
literal or unset `outDir`) on the workspace directory, plus a workspace
`index.html`. A directory that merely exists is never an output.

## Authorization and opaque IDs

`ProposalAuthorization.node_static_workspace` (optional, omitted when absent,
so every earlier authorization/frozen search is byte-identical):

```
NodeStaticWorkspaceAuthorization {
  install: { node_version, package_manager, package_manager_version, install_mode }
  workspaces: { "w_0": { cwd, output_root }, ... }   // static-qualified only
}
```

IDs are ordinal in sorted path order, meaningful only for this frozen source.
Nothing exists without an exact root scope and ≥1 static-qualified workspace.
The requester recomputes the inventory from the digest-verified extraction and
refuses any differing domain. This mapping is never serialized to a provider.

## Operation catalog and proposal

```
OperationDomain:     {"operation":"node_static_workspace@1","workspace_ids":["w_0"]}
OperationInvocation: {"operation":"node_static_workspace@1","workspace_id":"w_0"}
```

Only `propose_derivation` with exactly one invocation; strict schemas reject
path/cwd/argv/output/network/runtime/K fields. Unknown IDs are
`proposal_id_unauthorized`. The proposal schema/version is unchanged (serde enum
addition). Candidate-producer prompt v2 describes the operation; v1 bytes and
hash remain pinned. One-round/max-proposal limits are unchanged.

## Canonical D

```
[[runtime]] node = <exact>          [[runtime]] <yarn|pnpm> = <exact>
install  ato.process@1 exec  argv [mgr, install, --frozen-lockfile|--immutable]
         cwd "."           network "dependency-resolution" (requirement)
build    ato.process@1 exec  argv [mgr, run, build]   cwd <workspace>   (denied)
site     ato.browser@1 serve source workspace  root <workspace>/<out>
         entry index.html  spa_fallback true  cwd "."
port app.http from site
```

Install scope (source root, the one lockfile) and build scope (the resolved
workspace) are separate. A root script such as `yarn --cwd ./app build` is
never parsed or executed as shell. This D equals the known-D v2 builder's D
for the same resolved scope (tested). Frozen K must be HTTP observations on
`app.http` plus optional source identity, and is compiled unchanged.

Proposal searches remain `network = denied` (existing scope). The operation
adds no permission: under denial the install requirement stops at
`network_denied`. Compiler admission is not K; only Runtime + Verifier decide.

## Known-D (CandidateProducer OFF)

Unchanged: any workspace source still ends at
`preset_node_static_v2_workspace`. The optional "exactly one candidate" auto
route is not implemented in this slice (no measured app needs it).

## Not included

Arbitrary monorepo inference, root build-script parsing, Turbo/Lage,
manager-config interpretation, Yarn plugins, multi-service, source patches,
live LLM, Runtime Network receiver WASM rebuild in ato-api (the lib authority
is shared; deployment of the receiver is a separate change), deploy.
