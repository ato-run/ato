# Ato

Ato is a computing interface for preserving, sharing and continuing work from
a named point. Its semantic center is an evolving **Computation**:
`C ──α──▶ C′`. A **Capsule** seals an immutable, addressable continuation;
a **Run** resumes it and develops a new future. Records preserve evidence of
what happened. Physical processes, source trees, containers and snapshots are
ways to realize that work.

The Web service **ato.run** is the primary product surface. This repository
contains the runtime, CLI, Protocol/Adapter integrations and supporting tools
used by developer and automation workflows. English is the default language
of current documentation. [日本語の概要 / Japanese](README.ja.md).

## Read and build

| Goal | Start here |
|---|---|
| What is Ato? | [Computation](docs/concepts/computation.md) |
| Core Concepts | [Capsule / Contract K / Derivation D / Source](docs/concepts/application-contracts.md), [glossary](docs/glossary-reference.md) |
| Architecture | [Architecture map](docs/architecture/README.md) |
| Getting Started | [Build the CLI and verify a sample](docs/guides/getting-started.md) |
| Capsule Specification | [Accepted specifications and implementation-track contracts](docs/specifications/README.md), [bundle profiles](docs/capsule.md) |
| Formation Guide | [Form and verify an application](docs/guides/formation.md) |
| Runner / Hosting | [Execution paths and admission boundaries](docs/guides/runner-hosting.md) |
| Contributing | [Development workflow](CONTRIBUTING.md) |
| Operations | [Update, rollback, deployment identity and provisioning](docs/operations/README.md) |

[All documentation](docs/README.md) includes RFC status, historical evidence
and the complete inventory.

## The model

“Everything can be a Capsule” is an architecture principle: a computation
with a defined boundary, typed interactions and continuation semantics can
use the same interface. Applications, build work and agent tasks do not need
separate top-level semantic primitives. Setup and launch can use existing
tools such as Docker, Nix, scripts or agents.

The computation-root profile identifies a Capsule by `ComputationRef` and uses
wire v2. The portable application profile identifies the observable Contract
K by `ContractRef`; a Derivation D describes a way to realize it. Wire v3/v4
carry that profile. The transport file hash is distinct from either identity.
The [specification index](docs/specifications/README.md) marks which contracts
are accepted and which remain draft despite implemented code.

Compatibility means preserving K and producing a verified new realization.
It does not imply preserving the old implementation indefinitely. Equal K
does not by itself guarantee state continuity, safe side effects, a security
patch or a safe rollout. See [compatibility](docs/concepts/compatibility.md)
and [state continuity](docs/concepts/state-continuity.md).

## First local example

With the repository's pinned Rust toolchain:

```sh
cargo build -p ato-cli --bin ato --locked
mkdir -p .tmp/getting-started
target/debug/ato run samples/interop-static.capsule --no-open \
  --verification-receipt .tmp/getting-started/receipt.json
```

The [Getting Started guide](docs/guides/getting-started.md) explains the
input, receipt and failure boundary. This example uses the supplied static portable
application; Python, OCI, GPU, state and external services need their own
declared routes and host admission. Do not infer production availability or
general cross-host/process-memory restoration from a successful sample.

<!-- Compatibility anchors from the previous documentation entry. -->
<a id="main-use-cases"></a>
<a id="design-principle-everything-can-be-a-capsule"></a>
<a id="core-model"></a>
<a id="capsule--run--replay--continue"></a>
<a id="core--kernel-and-adapter"></a>
<a id="materialization"></a>
<a id="the-computation-theory-behind-it"></a>
<a id="basic-lifecycle"></a>
<a id="capsuletoml-example"></a>
<a id="project-scope"></a>
<a id="current-implementation-status"></a>

<!-- Additional Docsify slug aliases (including numeric prefixes). -->
<a id="capsule-run-replay-continue"></a>
<a id="core-kernel-and-adapter"></a>
