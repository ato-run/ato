# Run

## Overview

`ato run` is the front door for running a local directory, GitHub repository,
Store reference, or share URL right now. In the public docs, this page gives the
working model first and then points to RFCs for the deeper contract.

## How it works

At a high level, the flow is:

1. Normalize the input
2. Choose a runtime from `capsule.toml` or preview metadata
3. Resolve the required tools and runtimes
4. Freeze the execution plan with lock state and policy
5. Launch in a controlled environment

The detailed CLI surface and routing rules live in
[`ATO_CLI_SPEC.md`](rfcs/accepted/ATO_CLI_SPEC.md) and
[Core Architecture (JA)](core-architecture.md).

## Specification

- `ato run` MUST treat execution as ephemeral, not as persistent installation.
- `ato run` MUST resolve local path, GitHub repo, scoped Store reference, and share URL.
- required env MUST fail closed before process launch.
- runtime resolution SHOULD prefer pinned runtimes and tools recorded by lock state.
- `capsule.toml` is the primary authoring contract for local projects.

References:

- [`Repository README`](https://github.com/ato-run/ato/blob/main/README.md)
- [`rfcs/accepted/ATO_CLI_SPEC.md`](rfcs/accepted/ATO_CLI_SPEC.md)

## Design Notes

`run` stays the front door so the user does not need a different mental model
for each kind of software. The priority is not “what kind of thing is this?” but
“how do I run it through the same handle?”
