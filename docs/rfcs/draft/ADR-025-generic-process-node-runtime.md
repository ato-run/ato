# ADR-025 — Generic Process execution; Node, npm, pnpm and yarn

**Status**: proposed
**Context**: P0 Formation benchmark. After authored `exec` projection
(ADR-024) the most common first blocker was "this build provisions only
`python`": 8 of 20 Derivations declare Node. Code:
`lib/formation/src/{projection,intent}.rs`,
`apps/formation-worker/src/{job,local,executor,runtime_network,sandbox,build}.rs`.

## The decision

### Process is language-independent

A Derivation already says everything a process needs, in terms that are not
about any language: `ato.process@1` argv, cwd, env, ports, state, and
`[[runtime]]` requirements. There are two execution shapes — files served to
a browser, and a process — not one per language. So:

- no `NodeProcess`, `GoProcess`, `JavaProcess`, and no Node executor. Node
  runs through the same build sandbox, the same artifact, the same
  TemporaryRealization and Runtime process boundary as Python;
- a new intent lane, wire value `process`, is the generic process. What it
  runs comes from the Derivation's `[[runtime]]` declarations (each
  provisioned exactly) and the source's own `packageManager`, **never from
  the argv**: `argv = ["node", …]` with no `[[runtime]] node` gets no Node;
- Runtime Network requirements are shape-only: a process needs
  `runtime.process = true`; Node is a *toolchain provision*
  (`toolchain.node.<version>`), not an executor capability. There is no
  `runtime.node`.

### Python legacy identity

`python_process` stays exactly as it was. A process route whose declared
runtimes are Python only (or none) compiles through the v1 Python lane,
unchanged: same `ProgramIntent`, same `EffectiveBuildPlan`, same formation
key, same result shape and media type (golden tests, measured on the code
before this change, including Python and static routes with `exec` steps).
Only routes that declare another runtime use the generic lane, and those
were refused before, so no existing identity moves.

Declared toolchains travel under their own override key (`toolchain.<name>`)
rather than the v1 `runtime.python`, so the v1 lane's resolution is not
touched either.

### Runtimes: exact, declared

- Node: the exact version the Derivation declares, from
  `SUPPORTED_NODE_RUNTIME` (`20.20.2`, `22.14.0`), provisioned into
  `/opt/ato/toolchains/node/<version>` from nodejs.org. Anything else is
  `intent_unsupported_runtime`.
- Python: exact, from the existing catalog.
- A route may declare several (Python + Node: Node builds, Python serves).
  Each is an independent provision; they are provisioned in a fixed order
  (python, node, package manager), then the authored `exec` steps run.
- Provisioning is a platform prerequisite: in the plan (and its digest),
  never in the `DerivationRef`. The same D on another target plans another
  download of the same version.

### Package managers

- npm is the one the declared Node distribution ships. A `packageManager`
  of `npm@x` is not provisioned separately.
- pnpm and yarn: the version authority is the source's
  `package.json#packageManager` when it pins an exact version
  (`pnpm@10.4.1`, a Corepack `+sha512…` suffix ignored). Otherwise the
  Derivation may pin one with `[[runtime]] name = "pnpm"` (it may not
  contradict the source's exact pin). A source whose lockfile or
  `packageManager` names pnpm / yarn with no exact version from either is
  refused: `package_manager_version_unresolved`. `latest` is never used; no
  version is chosen on anyone's behalf.
- The pinned manager is installed with the declared Node's npm into its own
  shared prefix, `/opt/ato/toolchains/<pnpm|yarn>/<version>` (yarn 1 from
  `yarn`, yarn 2+ from `@yarnpkg/cli-dist`), and its reported version is
  checked. Not a per-attempt Corepack cache: the Run needs the same binary.
- The resolved manager is recorded in the intent (`package_manager`) and the
  plan (its provision step and PATH). Same I + D + R → same plan digest; a
  different pin in the source → a different plan, the same `DerivationRef`.

Native addons: a toolchain declared here is Node's, not a C compiler. A
package that compiles native code falls back to the host's `cc`, which the
build sandbox does not currently expose reliably (on Debian/Ubuntu `cc` is a
link through `/etc/alternatives`, which is not bound). Observed with Uptime
Kuma (`better-sqlite3`); recorded as a gap, not addressed here.

### Toolchain visibility: build and Run alike

- Build: the plan's `toolchain_path` (the provisioned `bin` directories)
  heads the build sandbox's own PATH, before `/usr/local/bin:/usr/bin:/bin`.
  A shell a step starts (`sh -c "npm run build && …"`) resolves `node`,
  `npm`, `pnpm` and `yarn` to the declared toolchains, never the host's.
- Run: the generic lane's intent carries the same PATH in its public env,
  which the Runtime launch applies over the host PATH it would otherwise
  pass through. The toolchain root is already read-only inside every Run
  sandbox. An authored `PATH` is the author's and is not overridden.
- Package-manager caches and stores are pointed away from `$HOME` (which in
  the build is the workspace, i.e. the artifact): the cache mount for a
  step that may use the network, the step's own `/tmp` otherwise. The Run
  gets `HOME=/tmp` (it has no home, and npm, pnpm and yarn all call
  `os.homedir()` before running anything) and `npm_config_cache=/tmp/.npm`,
  each only when the Derivation did not set it.

### `dependency-resolution` is an authorization class, not an allowlist

**`dependency-resolution` is an authorization class, not a destination
allowlist.** A step granted it today runs with the host's network,
unrestricted (`bwrap --share-net`). This ADR does not change that. What
holds: a step that did not declare a network gets none; a step that needs
one under a policy that denies it is refused before any step runs; the
worker's environment and secrets never enter the sandbox; the Run keeps its
own no-egress policy (TCP bind limited to its allocated host ports) whatever
network a build step had.

## Out of scope

Go, Java, Bun; source transport beyond 32 MiB; multi-service; new state
semantics; bindings; Docker socket; a Browser Contract for static routes;
destination allowlists for `dependency-resolution`; hosted Runner symlink
unpack and resolver alignment (ADR-023).
