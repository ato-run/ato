# Ato

Ato is an open-source runtime and computation model for verifiable, transferable software.

[ato.run](https://ato.run/) is a platform built on Ato for discovering, trying, saving, and sharing runnable software. This repository contains the runtime, CLI, adapters, and the model behind it. [About Ato](https://ato.run/about/) · [日本語](README.ja.md)

## What you can do

- Discover and try public software on [ato.run](https://ato.run/).
- Build the CLI and run a supplied portable application, then inspect its verification receipt.
- Explore how a shared contract can describe the observable requirements for different declared execution paths. Availability depends on the host and selected route.

## Try it

Explore the [live Discover experience](https://ato.run/) or [try 2048](https://ato.run/?view=discover&detail=discover%3Adiscover-2048). For a local developer example, build this checkout with its pinned Rust toolchain:

```sh
cargo build -p ato-cli --bin ato --locked
mkdir -p .tmp/getting-started
target/debug/ato run samples/interop-static.capsule --no-open \
  --verification-receipt .tmp/getting-started/receipt.json
```

The [getting started guide](docs/guides/getting-started.md) explains the fixture, receipt, and execution requirements. This example verifies a static application; it does not establish general cross-host portability.

## The model

A **Contract K** specifies observable conditions that a realization must satisfy. A **Derivation D** describes a declared way to realize those conditions. A **Capsule** is a sealed, immutable, addressable continuation point; a **Run** resumes it and evolves. A verification **receipt** records the selected D and the actual observations against K. It is evidence, not the computation itself.

The computation-root profile uses a `ComputationRef` and wire v2. The portable application profile uses a `ContractRef` and wire v3/v4; its transport file hash identifies bytes, not semantic identity. The portable application specifications are [implementation-track drafts](docs/specifications/README.md), not accepted RFCs. See [K and D](docs/concepts/application-contracts.md), [Computation](docs/concepts/computation.md), and the [theoretical background](docs/theory/capsule-process-model.md) for the detailed model.

## Status and limitations

Ato is experimental. This checkout includes computation objects, lifecycle commands, adapters, portable bundle handling and the supplied static verification example. Python, OCI, GPU, state, and external services have separate declared requirements and admission boundaries; code in this repository does not establish availability on ato.run production.

General recovery across heterogeneous hosts, process-memory restoration, and autonomous AI Formation are research or development work, not generally available capabilities. Ato does not promise that every application runs everywhere or replace environment tools such as Docker and Nix. Equal K alone does not guarantee state continuity or safe side effects. See [compatibility](docs/concepts/compatibility.md) and [state continuity](docs/concepts/state-continuity.md).

## Documentation and contact

- [Documentation index](docs/README.md), [specifications](docs/specifications/README.md), [formation](docs/guides/formation.md), and [runner and hosting](docs/guides/runner-hosting.md)
- [Contributing](CONTRIBUTING.md) and [GitHub issues](https://github.com/ato-run/ato/issues)
- [ato.run documentation](https://ato.run/docs/) and [contact](https://ato.run/legal/contact/)

<!-- Compatibility anchors from the previous documentation entry. -->
<a id="main-use-cases"></a><a id="design-principle-everything-can-be-a-capsule"></a>
<a id="core-model"></a><a id="capsule--run--replay--continue"></a>
<a id="core--kernel-and-adapter"></a><a id="materialization"></a>
<a id="the-computation-theory-behind-it"></a><a id="basic-lifecycle"></a>
<a id="capsuletoml-example"></a><a id="project-scope"></a><a id="current-implementation-status"></a>
<a id="capsule-run-replay-continue"></a><a id="core-kernel-and-adapter"></a>
