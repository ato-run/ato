# Sample applications

Status: Current

Each sample under [`samples/`](../../samples) is authoring input for `ato pack`
and a bundle `ato run` can execute. The bundle a sample produces is immutable:
the same `.capsule` bytes carry the same `ContractRef` and `DerivationRef`
everywhere. What differs between hosts is the Materialization, and the Run's
receipt records which one ran.

## OCI samples and the host platform matrix

| Sample | Route | Linux native Docker | macOS / Docker Desktop | Linux rootless Docker | Remote daemon |
|---|---|---|---|---|---|
| [`oci-service-group-proof`](../../samples/oci-service-group-proof) | `ato.oci@1` service group, two services, one Surface Port | `connector=direct` (verified) | `connector=relay`, `platform_emulated` for `linux/amd64` (verified) | `connector=relay` (same materialization; not verified on a rootless host) | refused: `remote_daemon_workspace_unsupported` |
| single-container `ato.oci@1` routes | one container | `connector=direct` (verified) | refused: `oci_container_relay_unsupported` (a single container has no group network for the relay to join) | refused the same way | refused: `remote_daemon_workspace_unsupported` |

`ato pack` prints the answer for the host it runs on, before anything starts:

```text
capability derivation_ref=sha256:… route=OCI Service Group platform=linux/amd64 connector=relay runnable=yes state=Advertised
capability reason=docker_bridge_unreachable_vm message=…
capability warning=platform_emulated message=…
```

`ato run` refuses before realizing anything, with the same reason code and a
sentence naming the action.

### Why the host decides the connector

A container's Port is logical. Where it is physically reachable is a fact about
the host:

- **Native Linux daemon (root):** the CLI shares the daemon's kernel, so the
  container's address on the Run's `--internal` network is reachable. The Run
  connects directly.
- **VM-backed daemon (Docker Desktop and equivalents):** the daemon runs in a
  VM, so container addresses exist only inside it. The Run starts one relay
  container on the same internal network and multiplexes the Ports over its
  stdio. The relay may only reach the `(service, guest_port)` pairs the
  Derivation declares.
- **Rootless daemon:** the container network is in a user namespace the CLI
  cannot enter. Same relay materialization, different reason code.
- **Remote daemon (`tcp://`, `ssh://`):** this host's workspace tree is not on
  the daemon's machine, so the Run is refused rather than built against the
  wrong tree.

The decision is made from observed facts (`docker info`, the context endpoint,
whether the CLI and daemon share a kernel, file-sharing configuration), never
from a product name, and it is recorded in the receipt.

## Running a sample

```sh
ato pack samples/oci-service-group-proof -o .tmp/oci.capsule
ato run .tmp/oci.capsule --no-open --verification-receipt .tmp/mac.json
```

`fully_satisfied: true` in the receipt means every observation of the sample's
Contract was satisfied by the actual run. The receipt also records:

- every service's container id and image,
- the connector (`direct` or `relay`),
- for a relay, the relay image digest and the relay container id,
- whether the platform ran under emulation.
