# Runtime vs runtime tools

Ato distinguishes between **primary runtimes** and **auxiliary runtime tools**.
They are modeled by two different mechanisms, and a thing belongs to exactly one
of them.

## Runtime-modeled

Primary runtimes execute the program. They are resolved through the runtime path
and recorded under `runtimes.<name>` in the lockfile (`RuntimeSection` /
`RuntimeEntry`).

- Deno
- Python
- Node
- Java
- .NET

## RuntimeToolSpec-modeled tools

Auxiliary tools prepare or launch the execution world (dependency resolution,
materialization, script running). They live in the
`RuntimeToolSpec` registry (`crates/capsule/src/contract/tools.rs`,
`REGISTRY`) and are recorded under `tools.<name>` in the lockfile
(`ToolSection`).

- uv
- pnpm
- yarn
- bun

## Why Deno is not in `RuntimeToolSpec::REGISTRY`

**Deno is not missing from runtime support, and its absence from the tool
registry is not a bug.** Deno is a primary runtime, so it is modeled as a
runtime: it appears under `runtimes.deno` in the lockfile, never `tools.deno`,
and `ToolSection` has no `deno` field. Its lock/runtime behavior is handled by
the Deno runtime path (`resolve_deno_runtime`, `generate_deno_lock`,
`deno_artifact_filename`, `supported_deno_platforms`).

This note exists because #31 ("register yarn/bun/deno/uv") read as if all four
should become `RuntimeToolSpec` tools. Only uv/pnpm/yarn/bun are tools; Deno is
a runtime. See #470 for the classification decision; #41 tracks the umbrella
roadmap.

Migrating Deno into `RuntimeToolSpec` is intentionally **not** done — the two
models are kept distinct at the registry boundary.
