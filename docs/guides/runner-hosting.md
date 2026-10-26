# Runner and Hosting

A Runner realizes authorized work under host-specific limits. It does not
define K or replace the Rust verifier. The Web product at ato.run is the main
product entry; the CLI serves developer and automation workflows. Runtime
semantics remain independent of that surface.

| Path | Start here | Evidence boundary |
|---|---|---|
| local portable execution | [Getting started](getting-started.md), `ato run` | actual local receipt for selected D |
| durable local Instance | `ato app import`, `start`, `inspect`, `stop`, `export`; [draft](../rfcs/draft/PORTABLE_LOCAL_INSTANCE.md) | Instance data and Run-scoped receipts; profile-specific snapshot support |
| user-owned Connected Runner | [Enrollment guide](connected-runner-self-host.md) | registration is distinct from runtime/isolation/storage admission |
| hosted portable execution | existing bundle intake → quarantine/validation → Hosted Run | deployed API/validator/Runner plus actual Run receipt |
| hosted source Formation | [Formation guide](formation.md) | source approval and formation result are distinct from Hosted Run verification |

Use `ato-connected-realization-worker`, not the retired `ato runner serve` or
`login` commands. Doctor output alone does not certify workload execution,
GPU support, public routing or durable storage. A logical HTTP Port is resolved
to that Run's dynamic endpoint; it need not be a file in the bundle.

Operational work starts with [Operations](../operations/README.md), especially
drain, owned-resource cleanup, binary identity and rollback. API/PWA deployment
and remote migrations belong to their owning repositories and environment
instructions. This repository's historical staging records do not authorize a
new deployment or feature activation.
