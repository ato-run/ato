# Samples

Status: Current technical reference.

This page describes the samples referenced from the README. Each entry states
what is committed, the exact command, and what the sample does not demonstrate.

## Whiteboard

**Status: runnable static sample.**

- Bundle: [`samples/whiteboard.capsule`](../../samples/whiteboard.capsule)
- Source: [`samples/whiteboard/`](../../samples/whiteboard)
- Command: `ato run samples/whiteboard.capsule`

The capsule serves a canvas whiteboard with pre-placed shapes from
`index.html`. Clicking adds a shape; dragging moves one; "Reset to the bundled
state" restores the initial shapes. Edits are kept in the browser's local
storage for that Run.

What it demonstrates: a sealed static bundle, a verified HTTP observation and
`proof.txt` body digest, and an editable starting state.

What it does not demonstrate: persistence across Runs. The local storage is
per browser origin and the Run is ephemeral; cross-run continuation belongs to
the Instance/snapshot path.

## Restore a Workspace

**Status: runnable static sample.**

- Bundle: [`samples/workspace.capsule`](../../samples/workspace.capsule)
- Source: [`samples/workspace/`](../../samples/workspace)
- Command: `ato run samples/workspace.capsule`

The page loads the bundled `state.json` (a small notes workspace), renders it,
and lets you add notes. Edits are kept in local storage; "Restore bundled
state" returns to the bundled data.

What it demonstrates: restoring a workspace from data carried inside the sealed
bundle. What it does not demonstrate: preserving edits across Runs or hosts.
That requires a durable local Instance and, where saved data must move, a
snapshot Contract; this static sample does not claim either.

## Compose Applications

**Status: runnable fixture with host requirements; not verified on macOS.**

- Source: [`samples/oci-service-group-proof`](../../samples/oci-service-group-proof)
- Command:

```sh
ato pack samples/oci-service-group-proof -o .tmp/oci-service-group.capsule
ato run .tmp/oci-service-group.capsule --no-open \
  --verification-receipt .tmp/oci-service-group.json
```

One `ato.oci@1` service group runs an internal backend and a web service. The
Contract observes the web surface and a request proxied to the backend. It
requires a native Linux/amd64 Docker host and the pinned images. OCI container
routes are refused on macOS, including Docker Desktop, so this sample was not
run in the environment used for the other three.

[`samples/portable-multi-process-authored`](../../samples/portable-multi-process-authored)
is a different case: one Application with two process Derivations
(`--derivation` selects one). It is multi-D, not multi-application composition.

## Contract-preserving Derivation

**Status: runnable static sample plus separate historical evidence.**

- Bundles: [`a.capsule`](../../samples/contract-preserving/a.capsule) and
  [`b.capsule`](../../samples/contract-preserving/b.capsule)
- Sources: [`a/`](../../samples/contract-preserving/a) and
  [`b/`](../../samples/contract-preserving/b)
- Command: `bash samples/contract-preserving/check.sh`

The two bundles are built from different source trees but share a behavioral K:
the requirement is only the `proof.txt` status and body digest, not the source
closure. The script runs both, prints the refs and checks that `contract_ref`
is equal while `derivation_ref` differs.

What it demonstrates: the same K can be satisfied by different implementations
and Derivations; K does not contain the source or the D. What it does not
demonstrate: the larger transformation experiment, state continuity or Hosted
execution. Those records are separate:

- [v0 contract-preserving transformation](../archive/2026/ops/contract-preserving-transformation-v0.md)
- [v1 stateful transformation](../archive/2026/ops/stateful-transformation-v1.md)
- [Derivation transformation](derivation-transformation.md)

The archived experiments run only on Linux with Bubblewrap, unprivileged user
namespaces and Python 3.12.7 at the CI fixture path:

```sh
cargo test -p ato-cli --test derivation_transformation --locked -- --nocapture
cargo test -p ato-cli --test stateful_transformation --locked -- --nocapture
```

## Environment matrix

| Sample | Needs | Verified here |
|---|---|---|
| Whiteboard | any host that runs a static bundle | yes (macOS, `ato run --no-open`) |
| Restore a Workspace | any host that runs a static bundle | yes (macOS, `ato run --no-open`) |
| Compose Applications | Linux/amd64, native Docker, pinned OCI images | no (macOS refused) |
| Contract-preserving Derivation (static) | any host that runs a static bundle + `ato` on PATH | yes (macOS) |
| Transformation experiments (archive) | Linux, Bubblewrap, unprivileged namespaces, Python 3.12.7 | not run here |

## See also

- [Getting started](getting-started.md) for the account-free static example.
- [Formation](formation.md) and [Runner and hosting](runner-hosting.md) for how
  a sample is produced and admitted.
- [Application contracts](../concepts/application-contracts.md) for K, D and the
  profile boundary.
