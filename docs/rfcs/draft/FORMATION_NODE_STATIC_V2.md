# node-static/v2 — canonical package-manager-aware static build

Status: 6b-A implementation contract; no deployment or new permissions.

`node-static/v1` and its npm K/D bytes and execution remain unchanged. The new
known-D frontend (`candidate_authoring`) retains v1 selection for npm, and routes
pnpm/Yarn facts to a separate `node-static/v2` authoring function. The historical
v1 selector/overrides remain unchanged; they cannot synthesize v2 without facts.

## Source qualification

Reuse NodeEvidence's lock detection, existing static-build detector, exact Node
catalog and `resolve_toolchains` / `ResolvedPackageManager`. New facts are
`devEngines.packageManager` and workspace presence. No second version resolver.
`packageManager` wins; contradictory exact devEngines declarations are refused.
No manager range, missing version, guessed generation or conflicting lockfiles.
Bun is deferred. Workspaces are explicitly refused in this slice, including a
pnpm workspace configuration file; no inferred subproject or output selection.

Manager redirect/hook configuration (`.yarnrc*`, `.npmrc`, `.pnpmfile.*`) is
conservatively refused rather than interpreted or allowed to replace the exact
provisioned manager. Root index.html is required.

The existing plain `vite build` + `vite preview`, no known server framework,
resolvable output rule is the conservative static-artifact qualification, not
package.json + any build script. Unqualified services/compound builds get typed
refusals. A qualified route is still only a candidate, never a verified app.

## Canonical D, before Runtime binding

The existing K template remains GET / = 200 + input identity. No K mutation or
functional product claim. D has exact `node` and `pnpm` or `yarn` runtimes and:

1. `ato.process@1 exec`: `[manager, install, --frozen-lockfile]` for pnpm/Yarn 1,
   or `[yarn, install, --immutable]` for Yarn >=2; cwd `.`;
   network `dependency-resolution` requirement (not permission).
2. `ato.process@1 exec`: `[manager, run, build]`; cwd `.`; network denied.
3. `ato.browser@1 serve`: source `workspace`, explicit relative output root,
   index.html, cwd `.`, SPA fallback; network denied.

No `workspace_build` inferred command expansion; no shell text is synthesized
into D. Source package scripts/lifecycles remain untrusted code run by the manager
under existing sandbox/effect/network admission. No model or patch generation.

The common bind/project_exec/lower_execution path validates and executes these
steps. Existing exact Node / manager provisioning is reused (Yarn >=2 uses
@yarnpkg/cli-dist, not an unversioned Corepack). A logical manager executable is
bound to its exact provisioned absolute path only in the physical build step;
it cannot fall back to a host PATH manager. This default binding only applies
when D explicitly requires that exact manager and does not author PATH. Legacy
source-only inference and explicitly authored PATH retain their prior semantics. D contains no toolchain homes,
host paths, Runtime IDs or credentials. No new executor/semantic primitive.

## Evidence and limits

T0–T21 plus local network-denied acceptance and the existing identity/plan
regressions precede remeasurement of the unchanged eight qualification archives.
Authoring reach, same-K verified coverage and functional success are separate.
Refused-before-K sources have no invented K/D. No 50/100 expansion, general
workspace inference, multi-service, Capsule identity decision or rollout here.
