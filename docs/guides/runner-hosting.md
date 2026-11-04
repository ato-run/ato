# Runner and Hosting

Status: Current technical reference.

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

## Dependencies and independence

The table records the dependencies each path is documented to have. "Yes" means
the path uses that dependency by design; it is not a deployment, availability or
feature-activation claim. Hosted rows still need the deployed API, validator and
Runner described in [Operations](../operations/README.md), and presence in code
does not prove that deployment.

| Path | Account required | Central API | Dependency acquisition | Network | Data location | Secrets |
|---|---|---|---|---|---|---|
| local CLI ephemeral run (`ato run <file.capsule>`) | No | No; the local Rust verifier checks K and writes the receipt | The selected route's runtime and sandbox must already be usable on the host; declared Model Sets must already be verified in `$ATO_HOME/cache`, and no upstream download is inferred | Only what the selected route declares; there is no service connection | The local host; the Run is ephemeral and leaves no durable Instance | No account credential; runtime inputs and Bindings only, and secret values are not persisted |
| durable local Instance (`ato app import` / `start` / `inspect` / `stop` / `export`) | No | No; local orchestration and Run receipts stay on the host | The imported bundle already carries its routes; the same host runtime/sandbox and local Model Set cache rules apply | None required by the lifecycle itself; route policy still applies | `$ATO_HOME` application and Instance directories on the user's host; Instance data and Run evidence stay local | No account credential; secret Binding is a later increment in the [draft](../rfcs/draft/PORTABLE_LOCAL_INSTANCE.md) |
| user-owned Connected Runner | Yes; enrollment uses a code from ato.run Settings | Yes; the Worker connects out to `--api-base` | Host capabilities are admitted separately ([enrollment guide](connected-runner-self-host.md)); the protocol does not provision or delete the machine | Outbound control-plane connection required; a public origin is optional and separately authenticated | The user's work root (`--work-root`); configured storage is not advertised as durable until admission succeeds | One-time enrollment token; device credentials are written 0600, revocable, and must not go into the service file |
| hosted portable execution | Yes; the ato.run account and its API credential | Yes | The deployed API/validator/Runner must admit the bundle's declared route, including its pinned runtime or OCI image | Required: the hosted service and that Run's dynamic HTTP endpoint | An operator-managed Hosted Runner, not the user's machine | API credential per origin; no secret is stored in the bundle, and Bindings are resolved at run time |
| hosted source Formation | Yes; ato.run submission and source approval | Yes | The shared portable builder on the formation host; a pinned OCI image is pulled only under `policy.network = "dependency_resolution"` | Required for source intake and formation; dependency resolution needs explicit policy | An operator-managed formation host and artifact store; result publication is separate from Hosted Run acceptance | API credential; no secret in the immutable manifest or evidence; source approval is a separate gate |

"Runs on my machine" and "continues without ato.run" are different claims. The
local CLI and a durable local Instance need no account and no central API, so
they can run and restart on the user's host alone. That independence ends at the
local root: the Instance store is local orchestration, its saved-data and secret
portability is not complete, and it cannot resume a Hosted Run. A Hosted Run or
Hosted Formation continues only while the hosted service and Runner it uses are
available; importing the same bundle elsewhere starts a separate Run or
Instance. Exporting or importing a bundle between surfaces moves immutable
transport bytes; it is not service independence and does not make either surface
continue the other. Do not claim preserved saved data or a hosted round-trip
without the evidence record that says so
([State continuity](../concepts/state-continuity.md)).

Use `ato-connected-realization-worker`, not the retired `ato runner serve` or
`login` commands. Doctor output alone does not certify workload execution,
GPU support, public routing or durable storage. A logical HTTP Port is resolved
to that Run's dynamic endpoint; it need not be a file in the bundle.

Operational work starts with [Operations](../operations/README.md), especially
drain, owned-resource cleanup, binary identity and rollback. API/PWA deployment
and remote migrations belong to their owning repositories and environment
instructions. This repository's historical staging records do not authorize a
new deployment or feature activation.
