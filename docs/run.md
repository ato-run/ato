# Run

## Overview

`ato run` is the front door for resolving source inputs with a recipe and
launching the resulting execution. Its command-level contract is an ephemeral
local production rehearsal, not durable app registration.

It accepts a local directory, GitHub repository, Store reference, local share
artifact, or canonical capsule handle. The CLI surface defaults
`path` to `.` and routes every input through the same run pipeline.

## Recipe selection

When running a local directory, Ato looks for a local recipe such as
`capsule.toml`. If one is not present, Ato may infer a basic recipe from source
evidence (language detection, entrypoint heuristics, etc.).

When running a remote source such as a GitHub repository, Ato may use:

- a recipe found in the source repository
- a recipe selected from the Store that targets this source
- an inferred recipe derived from source evidence

The selected recipe, together with the source inputs and user environment,
determines the execution identity for this launch.

> The following recipe-selection CLI forms are planned and not yet implemented:
>
> ```bash
> ato run github.com/org/repo --recipe web
> ato run github.com/org/repo --recipe ato.run/recipes/org/repo-web@1.0.0
> ato recipes github.com/org/repo
> ```

## How it works

At a high level, the flow is:

1. Parse the CLI and dispatch `Commands::Run`
2. Normalize special run inputs such as `capsule://...` handles and local share files
3. Apply environment assistance (`~/.ato/env/targets/<fingerprint>.env`,
   `--env-file`, SecretStore, and optional prompt)
4. Resolve or install the target and materialize an isolated run workspace
5. Execute the hourglass run pipeline:
   - Install
   - Prepare
   - Build
   - Verify
   - DryRun
   - Execute

The install phase now materializes dependency state into an isolated run
workspace before preparation starts, and the prepare phase can route either from
`capsule.toml` or from an authoritative `ato.lock.json` input.

The detailed CLI surface and routing rules live in
[`ATO_CLI_SPEC.md`](rfcs/accepted/ATO_CLI_SPEC.md) and
[Core Architecture](core-architecture.md).

## Specification

- `ato run` MUST treat execution as ephemeral, not as persistent installation.
- `ato run` MUST accept local paths, GitHub repositories, scoped Store references,
  local share artifacts, and canonical capsule handles.
- required env MUST fail closed before process launch, with CI refusing the
  interactive fallback path.
- saved per-target env values MAY be reused from `~/.ato/env/targets/`.
- `ato run` MUST pass through the standard hourglass phases rather than jumping
  straight from manifest load to execution.
- authoritative lock-backed runs MAY route from materialized `ato.lock.json`
  state instead of a local handwritten manifest.

References:

- [`Repository README`](https://github.com/ato-run/ato/blob/main/README.md)
- [`rfcs/accepted/ATO_CLI_SPEC.md`](rfcs/accepted/ATO_CLI_SPEC.md)
- [`rfcs/draft/run-install-semantics.md`](rfcs/draft/run-install-semantics.md)

## Design Notes

`run` stays the front door so the user does not need a different mental model
for each kind of software. The priority is not “what kind of thing is this?” but
“how do I run it through the same handle?” The current implementation keeps that
story while still doing fairly heavy work under the hood: install, dependency
materialization, verification, and receipt-aware execution.

The same recipe run against the same source on two different machines may produce
different `resolved_execution_id` values if the user environments differ. That is
expected and correct: Execution Identity captures the resolved launch world, not
just the declared intent.
