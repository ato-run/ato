# Repository Adapter and Workspace Semantics

Status: Accepted

Repositories are external authoring systems. `ato-adapter-repository` observes
source trees, ecosystem lockfiles, `package.json`, `pyproject.toml`,
Docker/other markers, and optional `capsule.toml`, then compiles them into one
`ato.workspace@1` `ComputationObject`.

`capsule.toml` is not canonical and has no compatibility guarantee with the
removed generic application manifest. The supported `[workspace]` fields are
only conveniences for selecting concrete workspace toolchain, entrypoint,
package manager, working directory, and semantic constraints. They compile
away.

The workspace residual owns future behavior: source closure identity, exact
runtime/toolchain constraint, entrypoint, working directory, semantic
environment values, safe secret binding IDs, writable topology, network
constraint, and its current phase. It is one concrete semantics, not a generic
workflow DSL.

Inference emits authoring evidence and handles ambiguity; it does not produce
LockDraft, ExecutionPlan, or compatibility projections. `ato lock` writes a
derived resolution receipt/cache whose identity-bearing choices are already in
the resulting computation.

Local paths and Git/GitHub sources use the same adapter path. CI tests Git
fetch through deterministic local repositories rather than requiring public
network availability.
