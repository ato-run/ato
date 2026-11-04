# Ato

Ato is a computing interface for preserving, sharing, and continuing work from
an addressable point. It combines a computation model with a runtime and CLI
for working with Capsules.

[ato.run](https://ato.run/) is the web service built on Ato for discovering,
trying, saving, and sharing runnable software. This repository contains the
runtime, CLI, adapters, and supporting tools. [About Ato](https://ato.run/about/)
· [日本語](README.ja.md)

## The problem

Someone you know made a small tool for their own use. You would like to use it
with your data, change one thing about it, and still open it next week, on
another machine, after the original author has moved on. Today software is
usually shared as source, an image, or setup instructions, so the recipient
has to reconstruct both the point the author already reached and the data and
services around it.

Work is often tied to a process, machine, source tree, or particular snapshot.
Moving it elsewhere can require rebuilding the environment and checking whether
the result still behaves as intended. Ato explores an approach that identifies
the work separately from the physical means used to realize it, such as a
process, container, or snapshot.

The design aims to support application sharing, continuing development work,
and handing off long-running agent tasks through a common interface. These are
use cases for the model, not a claim that every form of state or handoff is
supported today.

## What Ato is testing

- A **persistent continuation independent of its realization**: a sealed
  Capsule keeps its identity whether it is realized by Replay, a filesystem
  reconstruction, a checkpoint, or a VM snapshot.
- **Contract-indexed equivalence**: two realizations could count as the same
  point when they satisfy the same declared Contract K, even if their bytes
  differ. This is a hypothesis, not a behavior the implementation checks for
  arbitrary artifacts.

Neither is a claim of novelty; the open question is whether this combination is
a workable unit for sharing and continuing software. The
[research tour](docs/guides/research-tour.md) connects the model, the existing
evidence, prior work and the questions we want reviewed. The
[theory draft](docs/theory/capsule-process-model.md) describes the deeper
framing.

## Core concepts

- **Computation** is the work that can continue from its current point.
- **Capsule** is a sealed, immutable, addressable continuation point. It can
  be preserved and shared; the physical method used to realize it does not
  define its identity.
- **Run** resumes a Capsule and advances the computation. Runs can develop
  different futures from the same Capsule.

For portable applications, a **Contract** describes observable requirements,
a **Derivation** declares a way to realize them, and a verification **receipt**
records the observations for a Run. There are two identity profiles
(`ComputationRef` and `ContractRef`) that must not be reinterpreted as each
other. See [Computation](docs/concepts/computation.md) and
[application contracts](docs/concepts/application-contracts.md) for the model,
the profile map, and the decisions that remain open.

## Quick start

### Try the CLI locally (no account)

Install the Rust toolchain declared in [rust-toolchain.toml](rust-toolchain.toml),
then clone the repository, build the CLI, and run the supplied static
application bundle:

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

### Try ato.run (optional)

Explore public software on [ato.run](https://ato.run/) or
[try 2048](https://ato.run/?view=discover&detail=discover%3Adiscover-2048)
in your browser. ato.run is a separate product surface; using it is not
required for the local example above.

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

## Research and evidence

- [Research tour](docs/guides/research-tour.md): problem, model, profiles,
  data identity, verified results, independence and open questions.
- Archived experiments: [contract-preserving transformation v0](docs/archive/2026/ops/contract-preserving-transformation-v0.md)
  and [stateful transformation v1](docs/archive/2026/ops/stateful-transformation-v1.md).
  These are historical Linux evidence with recorded fixtures and limits; they
  were not re-measured for this overview.
- [Specifications](docs/specifications/README.md), including the accepted
  computation-root RFCs and the portable-application implementation track.

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

- [Research tour](docs/guides/research-tour.md) and the [theory draft](docs/theory/capsule-process-model.md)
- [Getting started](docs/guides/getting-started.md) and [Formation guide](docs/guides/formation.md)
- [Concepts](docs/concepts/computation.md), [application contracts](docs/concepts/application-contracts.md), and [glossary](docs/glossary-reference.md)
- [Architecture](docs/architecture/README.md) and [specifications](docs/specifications/README.md)
- [Runner and hosting](docs/guides/runner-hosting.md), [operations](docs/operations/README.md), and the [documentation index](docs/README.md)

## Contributing and license

See [Contributing](CONTRIBUTING.md) and [GitHub issues](https://github.com/ato-run/ato/issues).

Project-authored code is licensed under the [Mozilla Public License 2.0](LICENSE)
(`MPL-2.0`). Third-party code, dependencies, patches containing upstream code,
and materials with their own license notices retain their respective terms.
For example, the CPython fixture, WiX templates, and release workflow retain
their upstream licenses; the AODD skill retains its MIT license. The CLI's
[Apache license text](apps/cli/LICENSE-APACHE) is retained for the WiX template
and previously distributed versions. Previously granted licenses remain in effect.

This code license does not establish a new license for the Capsule Process
Model's theoretical documents.

| Material | Terms |
|---|---|
| Project-authored code | [MPL-2.0](LICENSE) |
| CLI Apache text retained for WiX and previously distributed versions | [apps/cli/LICENSE-APACHE](apps/cli/LICENSE-APACHE) |
| Third-party fixtures, templates, workflows and skills | their own notices, retained in place |
| Theory draft and specification documents | no new license is established by the code license; the original terms apply |

<!-- Compatibility anchors from the previous documentation entry. -->
<a id="main-use-cases"></a><a id="design-principle-everything-can-be-a-capsule"></a>
<a id="core-model"></a><a id="capsule--run--replay--continue"></a>
<a id="core--kernel-and-adapter"></a><a id="materialization"></a>
<a id="the-computation-theory-behind-it"></a><a id="basic-lifecycle"></a>
<a id="capsuletoml-example"></a><a id="project-scope"></a><a id="current-implementation-status"></a>
<a id="capsule-run-replay-continue"></a><a id="core-kernel-and-adapter"></a>
<a id="why-ato"></a><a id="try-atorun"></a>
