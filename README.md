# Ato

**A meta-runtime for running, preserving, and sharing your computing environment.**

Ato is an open-source computing interface for running, preserving and sharing
software, data and work state on computers you control, and for carrying the
work forward. You can use it on a desktop, home server, workstation or cloud
VM. This repository provides the foundation libraries, the CLI, the Runner and
the specifications of the computation model.

Ato's distinguishing feature is that it handles not only **execution** but the
**preservation and continuation of computation itself**.

Normally, moving an application to another environment means rebuilding its
source, dependencies and runtime, then restoring the necessary data and
settings separately. Even when the same application starts, the previous work
does not necessarily continue.

Ato treats these under one Computation model. By separating the conditions a
realization must satisfy from the way those conditions are met, it aims at
continuation that does not depend on a particular programming language,
framework, hardware, VM or container technology.

## Use cases

Ato is intended for everything from starting a single application to
continuing work that combines several pieces of software and data.

- **Share a development environment** — hand over a development environment and
  its work state so a teammate can continue on a different machine.
- **Deploy an application** — package software with the state it needs and run
  it locally or on a remote Runner.
- **Hand off an AI-agent task** — preserve the software an agent used, the data
  it produced and the state it was in, and continue on another environment or
  agent.
- **Restore data and work state** — reconstruct files, editing state and
  operation history by whatever method fits each of them.
- **Combine applications** — connect different software and data and run or
  preserve them as one piece of work.

## Quick Start

You can try several samples with the Ato CLI. They aim not only to start an
application but to **continue a computation from saved data or work state**.

### 1. Install

The Ato CLI is available through Cargo or from
[GitHub Releases](https://github.com/ato-run/ato/releases).

```bash
cargo install --git https://github.com/ato-run/ato.git ato-cli --locked
```

### 2. Verify the supplied static bundle

First, check the current verification path without an account. Clone the
repository and run the supplied bundle:

```bash
git clone https://github.com/ato-run/ato.git
cd ato
cargo build -p ato-cli --bin ato --locked
mkdir -p .tmp/getting-started
target/debug/ato run samples/interop-static.capsule --no-open \
  --verification-receipt .tmp/getting-started/receipt.json
```

`fully_satisfied: true` means the observations declared by this bundle were
satisfied for this Run. It does not demonstrate general cross-host portability
or state restoration.

### 3. Resume a whiteboard

Resume a whiteboard that already contains placed shapes from a saved Capsule:

```bash
ato run samples/whiteboard.capsule
```

The whiteboard opens in a browser with the pre-placed shapes. The sample is
intended to continue editing from that state and save a new continuation point.

### 4. Try more

| Sample | What you can try |
|---|---|
| [Restore a Workspace](docs/guides/samples.md#restore-a-workspace) | Restore files and editing state, then continue the work. |
| [Compose Applications](docs/guides/samples.md#compose-applications) | Combine several applications and data sets and run them together. |
| [Contract-preserving Derivation](docs/guides/samples.md#contract-preserving-derivation) | Keep the same K and realize or verify it with a different D. |

The [sample guide](docs/guides/samples.md) describes the supported
environment, commands and verifiable conditions for each sample.

> **Development status:** the whiteboard and multi-application samples are at
> the concept stage. The commands above are target operations, not a guarantee
> of current behavior.

## Services built on Ato

Ato is an open-source project intended to run locally and on computing
resources you manage, without depending on a particular cloud service.

[ato.run](https://ato.run/) is the official service under development on Ato.
It provides Discover and managed Runners to make software discovery, sharing
and execution easier.

Using Ato does not require an ato.run account.

## Documentation

- [Getting Started](docs/guides/getting-started.md)
- [Computation model](docs/concepts/computation.md)
- [Runner and hosting](docs/guides/runner-hosting.md)
- [Sample guide](docs/guides/samples.md)
- [Technical specifications](docs/specifications/README.md)
- [Documentation index](docs/README.md)

## Contributing

Ato is an open-source project. Bug reports, feature proposals and
implementation contributions are welcome.

- [GitHub Issues](https://github.com/ato-run/ato/issues)
- [GitHub Discussions](https://github.com/ato-run/ato/discussions)
- [Contributing Guide](CONTRIBUTING.md)

## License

Ato project code is released under the
[Mozilla Public License 2.0](LICENSE). Third-party code and materials with
their own license terms remain under those terms.

<!-- Compatibility anchors from the previous documentation entry. -->
<a id="main-use-cases"></a><a id="design-principle-everything-can-be-a-capsule"></a>
<a id="core-model"></a><a id="capsule--run--replay--continue"></a>
<a id="core--kernel-and-adapter"></a><a id="materialization"></a>
<a id="the-computation-theory-behind-it"></a><a id="basic-lifecycle"></a>
<a id="capsuletoml-example"></a><a id="project-scope"></a><a id="current-implementation-status"></a>
<a id="capsule-run-replay-continue"></a><a id="core-kernel-and-adapter"></a>
<a id="why-ato"></a><a id="try-atorun"></a><a id="license"></a>
