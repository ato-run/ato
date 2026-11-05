# Samples

Status: Current technical reference.

This page describes the samples referenced from the README. Each entry states
its actual status: a runnable fixture, a source example, recorded evidence, or a
concept. A command that is a target rather than a current capability is labelled
as such.

## Whiteboard

**Status: concept.** No `samples/whiteboard.capsule` is committed in this
repository yet.

Target behavior: start from a saved Capsule that already contains placed shapes,
open a browser whiteboard, continue editing, and seal the next point. The
command in the README (`ato run samples/whiteboard.capsule`) describes that
target; it does not currently run against a committed artifact.

The pieces it would exercise already exist: a sealed point, a run that is
verified against its Contract, and a browser-served surface. A committed
whiteboard bundle and its authoring source still need to be added.

## Restore a Workspace

**Status: source example.** The fixture is
[`samples/portable-stateful-notes`](../../samples/portable-stateful-notes):
a small Python HTTP application backed by a declared
`ato.state.filesystem@1` slot. It is a source fixture, not a packaged
`.capsule`; there is no `capsule.toml` in the directory yet.

What it demonstrates: an application whose write scope is a declared state slot,
not an implicit path. The intended continuation path is a durable local Instance
(`ato app import`, `start`, `stop`, `inspect`) whose Run keeps the Instance
metadata and saved data, rather than an ephemeral `ato run`.

What it does not demonstrate: general restoration of process memory, arbitrary
files, or external databases. Snapshot support is bounded to declared resources;
see [state continuity](../concepts/state-continuity.md).

## Compose Applications

**Status: runnable fixture with host requirements.**

[`samples/oci-service-group-proof`](../../samples/oci-service-group-proof)
declares one `ato.oci@1` service group with an internal backend and a web
service. The contract observes the web surface and a request proxied to the
backend. It requires a Linux/amd64 host with Docker and the pinned images.

```sh
ato pack samples/oci-service-group-proof -o .tmp/oci-service-group.capsule
ato run .tmp/oci-service-group.capsule --no-open \
  --verification-receipt .tmp/oci-service-group.json
```

What it demonstrates: several services realized as one Application under one
Contract, with declared resource limits and a declared filesystem state slot.
What it does not demonstrate: general composition of arbitrary applications or
cross-host migration.

[`samples/portable-multi-process-authored`](../../samples/portable-multi-process-authored)
is a different case: one Application with two declared process Derivations
(`--derivation` selects one). It is multi-D, not multi-application composition.

## Contract-preserving Derivation

**Status: recorded Linux evidence, not a portable sample bundle.**

The transformation experiments keep a frozen K and verify a candidate built
from a different source and Derivation. They are recorded in the archive, with
fixtures, negative cases and prerequisites:

- [v0 contract-preserving transformation](../archive/2026/ops/contract-preserving-transformation-v0.md)
- [v1 stateful transformation](../archive/2026/ops/stateful-transformation-v1.md)

Reproducing them requires Linux with Bubblewrap, unprivileged user namespaces
and Python 3.12.7 at the fixture path used by CI:

```sh
cargo test -p ato-cli --test derivation_transformation --locked -- --nocapture
cargo test -p ato-cli --test stateful_transformation --locked -- --nocapture
```

What it demonstrates: K preserved while source and D change, with the negative
case (a behavior break) rejected. What it does not demonstrate: restart
persistence, Hosted execution, or a safe rollout. See
[Derivation transformation](derivation-transformation.md).

## Environment matrix

| Sample | Needs | Runs today |
|---|---|---|
| Whiteboard | committed capsule (not yet present) | no |
| Restore a Workspace | source fixture + future packaging | source only |
| Compose Applications | Linux/amd64, Docker, pinned OCI images | yes, subject to admission |
| Contract-preserving Derivation | Linux, Bubblewrap, unsigned user namespaces, Python 3.12.7 | Linux test only |

## See also

- [Getting started](getting-started.md) for the account-free static example.
- [Formation](formation.md) and [Runner and hosting](runner-hosting.md) for how
  a sample is produced and admitted.
- [Application contracts](../concepts/application-contracts.md) for K, D and the
  profile boundary.
