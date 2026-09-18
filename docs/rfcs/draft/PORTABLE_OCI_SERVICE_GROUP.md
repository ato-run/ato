# Portable OCI Service Group

Status: draft implementation contract  
Date: 2026-09-18  
Tracking: ato-run/ato-api#630 (P2 ato-api#634, P3 ato-api#635, P4 ato-api#636)

## Decision

One Derivation may realize its Application with several cooperating OCI
containers on one Runner. This is an **evaluator capability of `ato.oci@1`**,
not a new application kind, execution ABI, or Semantic Core object.

A group is expressed with objects that already exist:

| Concept | Bound representation |
|---|---|
| service | one `BoundStep` with `protocol = "ato.oci@1"`, `op = "serve"` |
| service Port | one `BoundPort` with `from = <step id>` |
| service state | `BoundStep.state` naming a `BoundDerivation.state` slot |
| service secret | `BoundStep.bindings` naming an Application Binding |

`BoundStep` gains three additive fields — `runtimes`, `state`, `bindings` —
each omitted from canonical JSON when empty. Every Derivation formed before
this contract therefore keeps its exact bytes and `DerivationRef`; a
regression test pins a staging-published `ContractRef`/`DerivationRef` pair.

`[[derivation.service]]` in `ato.capsule/2` is authoring shorthand. It
compiles to the objects above and to nothing else. A future general grammar can
move to typed `derivation.step` without changing identity.

Multiple Derivations and multiple services are different things: Derivations
are alternative routes to the same Contract; the services of a group run
together inside one selected route.

## Profile (v0)

A route with more than one serving step is admitted only when:

- it has 2–4 steps, each `ato.oci@1`/`serve`, each id a single RFC 1123 DNS
  label (it becomes the container's network alias);
- the route-level `runtimes` holds only the shared `oci.platform`; each step
  holds a digest-pinned `oci.image` and positive `oci.memory_bytes`,
  `oci.cpu_limit_millis`, `oci.pids_limit`, optionally `oci.entrypoint` and
  `oci.workspace_mount`, and no other key;
- per-service and aggregate limits stay within 4 GiB memory, 4000 CPU
  millis and 1024 pids (`ato_ipc::oci_service_group`, the single authority for
  both the profile and the launch spec);
- there are 1–8 Ports, each with a guest port and served by a declared step;
  exactly one Port is the Application Surface Port and uses `ato.http@1`; all
  others use the payload-opaque `ato.tcp@1`; every step serves at least one
  Port so its readiness can be probed;
- at most one filesystem state slot exists, visible to exactly one step and
  not mounted over that step's workspace;
- every Application Binding is visible to exactly one step; environment-name
  collisions are checked per service, so siblings may reuse a name;
- `effects = "pure"`, with no workspace build or compiler.

Host networking, privileged mode, the Docker socket, arbitrary host mounts and
runtime builds have no field in the model and cannot be expressed.

A single-step route with any step-scoped field is refused: those fields
belong to groups only.

Services start in authored order, each after the previous one is ready, and
stop in reverse order. There is no dependency DAG in v0.

## Validator report

The Hosted validator reports a group as `realization = "oci_service_group"`.
Route-level `argv`/`cwd`/`env` are empty, `port` names the Surface Port and
`guest_port` is null. `services[]` carries each service's runtimes, argv,
cwd, env, Ports (`exposure = surface | internal`), state keys and Binding ids.
An API that does not know the realization rejects the report and fails
closed.

## Launch

- The Runner lease kind stays `runtime_launch`. A group travels as
  `launch_spec.protocol = "ato.runtime-launch-spec.v2"`; v1 specs keep their
  type, bytes and digest. A Runner build without v2 refuses the spec before
  anything is spawned.
- Scheduling requires `execution_abi=oci` and
  `runtime_feature=oci_service_group_v1`. Only Runners that can contain OCI
  workloads (native Linux with Docker) and understand v2 advertise the feature.
- The group's summed CPU is reserved atomically on its one Run lease through
  the existing `runtime_cpu_request` path. Memory and pids are enforced as
  per-container hard limits only; this contract does not claim memory
  admission until the scheduler reserves memory.
- The Runner creates one `--internal` bridge per Run, owned by the group, and
  starts each service with its network alias and the existing OCI hardening
  (`--pull=never`, `--read-only`, `--cap-drop=ALL`, `no-new-privileges`, pid,
  memory and CPU limits, per-service environment file removed after launch).
- Only the Surface Endpoint gets a loopback forwarder. Internal Endpoints get
  no forwarder and no host listener. Services of one group are the same
  Application trust boundary and are not isolated from each other by Port.
- If any service fails to become ready or exits, the whole group is stopped
  in reverse order and the Run fails. Restart policy belongs to P2.

## Guarantees and non-guarantees

The Runner host is a trusted execution substrate. v0 guarantees, for an
internal Endpoint: no Port forwarder, no host listening port, no reachability
from the ato.run ingress or from another group, and no route from the
container to ordinary external destinations. It does **not** claim that the
host itself cannot reach a container address; strict host and metadata egress
enforcement is completed with runtime egress policy (P4).

## Decisions fixed for the following steps

These are design commitments for P2–P4. They are not implemented by this
contract.

**Volume residency (P3).** An Instance state slot maps to a `volume_ref` with
`residency { runner_id }`. The physical volume is keyed by the durable state
slot identity, not by a raw `(instance_id, state_key)` pair. A launch spec
carries only `volume_ref` and `writer_fence`, never a host path. When the old
writer's stop cannot be confirmed, the slot is not moved to another Runner.

**Always-on authorization (P2).** The Coordinator issues an authorization
expiry for an `always_on` Run. The Runner requests renewal about every three
minutes and the Coordinator extends the expiry to now + 15 minutes. After an
explicit stop or revocation, renewal is refused. On expiry the Runner stops
the workload gracefully. **The Runner never extends its own authority.**
Public tries keep their fixed 180-second limit.

**Fixed TCP (P4).** Two layers: a stable allocation
`(allocation_id, runner_id, ip, protocol, port, instance_id)`, assigned by
operators only and audited, and an active binding
`(allocation_id, run_id, generation)`. The generation switches only after the
previous Run's stop is confirmed, so the public address never reaches a
stale Run.

## Deferred

- Importing a limited Compose file into this typed form. It does not advance
  the Mail acceptance and follows a working group on staging.
- Durable running volumes, restart policy, runtime egress, fixed TCP and
  private Bindings (P2–P4).
