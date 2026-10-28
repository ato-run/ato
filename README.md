# Ato

Ato is a computing interface for preserving, sharing, and continuing work from
an addressable point. It combines a computation model with a runtime and CLI
for working with Capsules.

[ato.run](https://ato.run/) is the web service built on Ato for discovering,
trying, saving, and sharing runnable software. This repository contains the
runtime, CLI, adapters, and supporting tools. [About Ato](https://ato.run/about/)
· [日本語](README.ja.md)

## Why Ato?

Work is often tied to a process, machine, source tree, or particular
snapshot. Moving it elsewhere can require reconstructing the environment and
checking whether the result still behaves as intended. Ato explores an
approach that identifies the work separately from the physical means used to
realize it, such as a process, container, or snapshot.

The design aims to support application sharing, continuing development work,
and handing off long-running agent tasks through a common interface. These
are use cases for the model, not a claim that every form of state or handoff
is supported today. The example below shows the current static application
verification path.

## Core concepts

- **Computation** is the work that can continue from its current point.
- **Capsule** is a sealed, immutable, addressable continuation point. It can
  be preserved and shared; the physical method used to realize it does not
  define its identity.
- **Run** resumes a Capsule and advances the computation. Runs can develop
  different futures from the same Capsule.

For portable applications, a **Contract** describes observable requirements,
a **Derivation** declares a way to realize them, and a verification **receipt**
records the observations for a Run. See [Computation](docs/concepts/computation.md)
and [application contracts](docs/concepts/application-contracts.md) for the
model and its profiles.

## Quick start

### Try ato.run

Explore public software on [ato.run](https://ato.run/) or
[try 2048](https://ato.run/?view=discover&detail=discover%3Adiscover-2048)
in your browser.

### Try the CLI

Install the Rust toolchain declared in [rust-toolchain.toml](rust-toolchain.toml),
then clone the repository, build the CLI, and run the supplied static application
bundle:

```sh
git clone https://github.com/ato-run/ato.git
cd ato
cargo build -p ato-cli --bin ato --locked
mkdir -p .tmp/getting-started
target/debug/ato run samples/interop-static.capsule --no-open \
  --verification-receipt .tmp/getting-started/receipt.json
```

The CLI validates the bundle, realizes its declared route, checks the source
identity and HTTP response against the Contract, and writes a receipt. On
success, inspect `.tmp/getting-started/receipt.json` for the selected
Derivation and observations; `fully_satisfied` is `true` when all required
observations are satisfied. `--no-open` verifies and exits without opening a
browser. See [Getting started](docs/guides/getting-started.md) for details,
other routes, and failure boundaries. Published CLI artifacts are listed in
[GitHub Releases](https://github.com/ato-run/ato/releases).

For example, inspect the receipt with Python 3:

```sh
python3 -c 'import json; r=json.load(open(".tmp/getting-started/receipt.json")); print("fully_satisfied:", r["fully_satisfied"]); [print(o["id"] + ":", o["outcome"]) for o in r["observations"]]'
```

```text
fully_satisfied: True
app-proof: satisfied
source-identity: satisfied
```

This static example does not demonstrate general cross-host portability or
restoration of process memory.

## Current status

Ato is experimental. This repository includes computation objects, lifecycle
commands, adapters, portable bundle handling, and the static verification
example above. Python, OCI, GPU, state, and external services require their
own declared routes and host admission; their presence in the code does not
establish availability on ato.run production.

General recovery across different hosts, process-memory restoration, and
autonomous AI Formation are research or development work. Ato does not
promise that every application runs everywhere or replace tools such as
Docker and Nix. Matching a Contract alone does not guarantee state continuity
or safe side effects. See [compatibility](docs/concepts/compatibility.md) and
[state continuity](docs/concepts/state-continuity.md).

## Roadmap and work tracking

Work is tracked in [GitHub Issues](https://github.com/ato-run/ato/issues) (and
GitHub Projects when a board is used). There is no Markdown roadmap in this
repository; [the archived roadmap](docs/archive/2026/legacy/OPEN_ISSUES_ROADMAP.md)
is historical evidence and is not updated.

## Documentation

General user documentation is owned by the
[ato.run documentation](https://ato.run/docs/). This repository owns the
technical documentation for the runtime, CLI, specifications and operations
([documentation index](docs/README.md)).

- [Getting started](docs/guides/getting-started.md) and [Formation guide](docs/guides/formation.md)
- [Concepts](docs/concepts/computation.md), [application contracts](docs/concepts/application-contracts.md), and [glossary](docs/glossary-reference.md)
- [Architecture](docs/architecture/README.md) and [specifications](docs/specifications/README.md)
- [Runner and hosting](docs/guides/runner-hosting.md), [operations](docs/operations/README.md), and the [documentation index](docs/README.md)

## Contributing and license

See [Contributing](CONTRIBUTING.md) and [GitHub issues](https://github.com/ato-run/ato/issues).

There is no repository-wide license file in the root. Some components
state their licenses in their manifests; for example, the
[CLI manifest](apps/cli/Cargo.toml) declares `Apache-2.0 OR MPL-2.0`, and the
[CLI license file](apps/cli/LICENSE) contains Apache 2.0. Check the relevant
component's license information; a repository-wide grant is not specified here.

<!-- Compatibility anchors from the previous documentation entry. -->
<a id="main-use-cases"></a><a id="design-principle-everything-can-be-a-capsule"></a>
<a id="core-model"></a><a id="capsule--run--replay--continue"></a>
<a id="core--kernel-and-adapter"></a><a id="materialization"></a>
<a id="the-computation-theory-behind-it"></a><a id="basic-lifecycle"></a>
<a id="capsuletoml-example"></a><a id="project-scope"></a><a id="current-implementation-status"></a>
<a id="capsule-run-replay-continue"></a><a id="core-kernel-and-adapter"></a>
