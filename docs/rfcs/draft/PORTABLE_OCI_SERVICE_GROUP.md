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
| service Binding | `BoundStep.bindings` naming an Application Binding |

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
- a route without an external network Binding has `effects = "pure"`; a route
  with `ato.tcp-egress@1` has `effects = "requires-confirmation"` or
  `"non-repeatable"`;
- there is no workspace build or compiler.

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

These are design commitments for P2–P4. The volume commitments are implemented
by Steps ②b/②c below. The authorization and network commitments are implemented
by Step ③ below.

**Volume residency (P3).** An Instance state slot maps to a `volume_ref` with
`residency { runner_id }`. The physical volume is keyed by the durable state
slot identity, not by a raw `(instance_id, state_key)` pair. A launch spec
carries only `volume_ref` and `writer_fence`, never a host path. When the old
writer's stop cannot be confirmed, the slot is not moved to another Runner.
The first slice of this is specified below (Runner-local persistent volumes).

**Execution authorization (P2).** The Coordinator issues renewable execution
authority for ato-managed compute and for a self-hosted Run whose owner
explicitly delegated execution management through `always_on`. The Runner
requests renewal about every three minutes and the Coordinator extends the
expiry to now + 15 minutes. After an explicit stop or revocation, renewal is
refused. On expiry the Runner stops the workload gracefully. **The Runner
never extends its own authority.** An independent maximum-duration policy may
end the same Run earlier; public tries keep their fixed 180-second limit.

**Fixed TCP (P4).** Two layers: a stable allocation
`(allocation_id, runner_id, ip, protocol, port, instance_id)`, assigned by
operators only and audited, and an active binding
`(allocation_id, run_id, generation)`. The generation switches only after the
previous Run's stop is confirmed, so the public address never reaches a
stale Run.

## Runner-local persistent volumes (Step ②b)

Status: implemented on staging behind a control-plane flag.

**Two kinds of slot.** A state slot is either revision-backed (the existing
model: its truth is the State Revision, restored into a per-Run working copy
and packed back after a confirmed stop) or runner-volume-backed (its truth is
a directory on one Runner, mounted in place and written by the application
directly). For a volume-backed slot a State Revision is only a seed, and
later a checkpoint (②c); `head_revision_id` is not "the latest data".

**Choosing the mode.** A control-plane policy, not part of K or D. While the
flag `PERSISTENT_STATE_VOLUMES` is on, the state slot of an OCI service group
switches to runner-volume at its next launch, seeded from the head revision
at that moment. The switch is recorded on the slot once and never reversed.
With the flag off nothing changes.

**Control plane.** `instance_state_volumes(volume_ref, state_slot_id UNIQUE,
runner_id, status, seed_revision_id, capacity_bytes, usage_bytes, …)`.
`status` is `provisioning | ready | missing`. Quarantine stays on the slot
(Step ②a) and is not duplicated on the volume. The first volume for a slot is
claimed with an insert that conflicts on `state_slot_id`, so concurrent
launches that picked different Runners still create exactly one volume; the
loser does not launch.

**Placement.** A slot with a volume is launched only on its resident Runner.
If that Runner is offline, drained or lacks the capability, the launch fails
with a typed error and no lease; it never falls back to another Runner and
never creates an empty volume elsewhere. A Runner must advertise
`execution_abi=oci`, `runtime_feature=oci_service_group_v1` and
`runtime_feature=runner_persistent_volume_v1`.

**Wire.** `ato.runtime-launch-spec.v3` is the v2 group with
`state_attachments[]` of `{state_key, mount_target, access = read_write,
writer_fence, backing: {kind: runner_volume, volume_ref, capacity_bytes,
initialize_from_revision_ref}}`. A new protocol rather than a v2 field
because the attachment means something different. v1 and v2 bytes and
digests are unchanged; a Runner without v3 refuses it by protocol before
anything is materialized, and the control plane never selects one.

**Runner.** Volumes live under a host-wide root
(`<root>/<runner_id>/volumes/<volume_ref>/{metadata.json,data}`), shared by
every slot worker of the host and outside any lease directory. A volume is
provisioned once — temporary directory, one-time seed, fsync, atomic rename —
then reported ready. A volume recorded ready that is absent or does not carry
this Runner's metadata is reported `missing` and the launch refused; it is
never rebuilt from an older revision. Recovery from a missing volume is an
explicit, later operation.

**Stop.** Unchanged from ②a: only a confirmed stop releases the writer, and
for a volume-backed slot the release does not pack or commit. An unconfirmed
stop quarantines the slot and keeps the volume. A host-local lock keeps two
attachments on one host apart; it is not evidence that a previous workload
stopped.

**Capacity.** `capacity_bytes` is admitted per volume: the Runner refuses to
start when the filesystem cannot offer what the volume may still grow by plus
a reserve, or when the volume already exceeds its capacity, and reports usage
after every stop. The State Artifact size limit does not apply to volumes.
There is no hard quota yet: an application can exceed its capacity while it
runs. **A hard quota is a production gate**; volumes stay staging-only until
it exists. Host reboot and Docker daemon failure are also production
acceptance items not exercised on the shared staging host.

**Consumers of the head revision.** Export, import, snapshot and schema
update paths that read or replace a slot's head refuse a volume-backed slot
(`state_volume_backed_checkpoint_unavailable`) until checkpoints exist (②c).

## Volume operations: checkpoint, restore, delete (Step ②c)

Status: implemented on staging.

**One handover order.** Every operation on a volume, and every new Run after
one, follows: stop input to the current Run (its route drains) → the current
Run's stop is confirmed and its writer released (②a; lease expiry, fences and
host locks are never taken as a stop) → the operation takes the next writer
generation by CAS → the resident Runner performs it with no workload running
→ its report releases the writer. While an operation is active no Run starts
(`state_volume_maintenance`); an operation never falls back to another Runner.

**Operations** (`instance_state_volume_operations`, one active per slot; lease
kind `state_volume_maintenance`):

- *checkpoint* packs the stopped volume and commits it as a State Revision —
  the only revision a volume-backed slot can take. It becomes the head and the
  volume's latest checkpoint, recorded with the writer generation it was taken
  under.
- *restore* writes a checkpoint beside `data/` and swaps it in with one atomic
  exchange; the target becomes the head.
- *delete* renames the volume out of the Runner's store in one step, removes
  it, and forgets the volume and the head. This is "Delete data"; removing an
  App is refused while a volume holds its data (`state_volume_retained`).

A Runner that dies mid-operation is recovered as a confirmed stop; the
operation is `interrupted` and the volume holds either the old or the new
contents, never a mix. Reports must match the operation, its lease, Runner,
Run and generation; anything older is refused.

**Export and clone.** Saved data is exported only from a checkpoint taken
after the last writer (`state_volume_checkpoint_required` otherwise). Importing
that export creates an independent Instance whose first launch seeds its own
volume from the imported revision.

**Limits.** A checkpoint is a single State Artifact (64 MiB); larger ones fail
typed (`state_checkpoint_too_large`) until chunked transfer exists.

## Always-on and network controls (Step ③)

Status: implementation contract.

### Control-plane responsibility boundaries

The runtime control plane keeps five meanings separate even when they share a
control exchange:

- **Assignment** records which Runner and incarnation handles a Run. Its claim
  deadline governs startup, not an already-running process.
- **Ownership** fences state writers, volumes and physical slots. It moves only
  after exact old-owner stop evidence or physical isolation, never because a
  timestamp elapsed.
- **Grant** is authority from one issuer for named actions on one resource.
  Execution, ato.run connectivity, state writes and Bindings are separate
  grants; expiry affects only the scope that was granted.
- **Observation** records Runner reachability, Run progress and route use. A
  stale observation may trigger reconciliation but is not proof of physical
  stop.
- **Policy/deadline** decides when to request work such as startup, idle sleep,
  maximum-duration stop or stop-ACK reconciliation. It does not itself release
  ownership.

The resulting invariants are:

```text
connection grant expiry != Run stopped
execution grant expiry  != stop confirmed
stop confirmed          != state commit succeeded
assignment ended        != physical resource released
```

An authenticated clean stop acknowledgement settles the exact Run's state
grants with their recorded writer fences. A terminal assignment without such
evidence quarantines ownership. A claim deadline never performs either action.

### Always-on policy and renewable authorization

`always_on` is mutable Instance policy, not part of K, Application or D. The
Coordinator stores the desired state (`running | stopped`) separately from the
observed Run state. An explicit stop commits `desired_state = stopped` before
requesting a Run stop, so a concurrent failure report cannot restart it. The
idle-sleep sweep skips an Instance whose mode and desired state are
`always_on/running`.

The Coordinator is the only restart authority. An abnormal exit creates a new
Run, lease and route generation through the ordinary launch path. The Runner
never restarts a container or extends its own authorization. Consecutive
failures use delays of 5 seconds, 30 seconds, 2 minutes, 10 minutes and 30
minutes; a sixth failure before 15 minutes of healthy operation leaves the
Instance `degraded` until an owner explicitly starts it again.

A lease command carries `execution_authorization` outside the digested launch
spec when execution occurs on ato-managed compute, or when `always_on`
expresses explicit delegation to the Coordinator. About every three minutes
the Runner renews through the lease control plane. A successful response
advances the expiry to Coordinator time + 15 minutes. Stop, revocation,
policy-generation mismatch for delegated always-on execution, wrong Runner or
a terminal assignment refuses renewal. A self-hosted on-demand Run receives
no such grant merely because ato.run provides a route to it.

The Runner converts the response's `server_time` and `expires_at` into a
monotonic local deadline; wall-clock changes cannot extend it. A transient
control-plane failure is tolerated only until that deadline, after which the
workload is stopped before the failure is reported. `max_duration_secs`, idle
policy and renewable execution authorization are independent stop conditions;
the first one reached requests/causes the stop without changing the meaning of
the others. Public tries therefore remain fixed at 180 seconds while still
requiring renewable authority when they consume ato-managed compute.

### External TCP Binding and egress grant

External connectivity reuses the Application Binding boundary. An Application
declares `protocol = "ato.tcp-egress@1"`; exactly one service references that
Binding. D therefore records which service needs the connection, while the
actual endpoint and permission remain mutable Instance input and do not alter
the Contract, Application or Derivation identity.

The first grant version contains an exact numeric IPv4 address or IPv4 CIDR
and one or more TCP ports. IPv6, hostname resolution, UDP, unrestricted
Internet and implicit `allow_all` are unsupported. A grant never permits
loopback, link-local, private, multicast or cloud metadata destinations, even
when a broader CIDR contains one. Restore and clone copy the declaration but
not the grant; the new Instance starts unbound.

The hosted Runner keeps the group-only `--internal` bridge and adds a separate
egress bridge only to the service that owns the Binding. A host-wide network
broker installs a default-deny forwarding policy for that bridge before the
container can join it. Failure to install or recover the policy refuses the
Run; removing `--internal`, sharing the host network or falling back to the
ordinary Docker bridge is never an error recovery path. Policy handles are
installed idempotently at host startup, while the Run journal records the
grant generations needed for recovery. The broker records grant and
generation metadata, never packet payloads or Binding secrets.

On Linux, Runner-owned egress interfaces use the reserved `atoe` prefix. A
startup preflight idempotently installs an INPUT default-deny rule for that
prefix and opens only the SOCKS5 broker on TCP 1080. This requires
a narrowly scoped privileged firewall helper (for example, exact sudoers rules
for these idempotent commands); the worker and workloads do not receive
`CAP_NET_ADMIN`. A Runner without the helper fails startup instead of
advertising egress capability. Every egress bridge has a distinct gateway, so
the same fixed broker Port does not merge grants or make a broker reachable
from another Run network.

This raw TCP path is distinct from the existing HTTP CONNECT proxy. Direct MX
delivery and a DNS policy are not implied by this version; staging acceptance
uses an operator-managed fixed-address sink.

### Fixed TCP allocation and active generation

A fixed TCP allocation is operator-created and Runner-affine. It names an
existing `ato.tcp@1` `BoundPort`; it does not add a public-port semantic to D.
The Coordinator stores the stable allocation independently from its active
Run binding. Restore and clone receive neither.

The host-wide network broker owns the public socket. It registers a Run target
as pending, then atomically activates it only for a generation newer than the
current one after the Coordinator has confirmed the prior Run stopped. A
deregister request includes the exact Run and generation, so stale cleanup
cannot remove the current target. During handover the stable listener has no
target and refuses new connections rather than forwarding to an old Run.
Existing connections receive a bounded drain before they are closed.

The Runner advertises the egress and fixed-TCP capabilities only when the
broker, firewall backend, configured bind-address/port allowlist and required
host privileges pass startup preflight. An allocation outside that allowlist
is refused before workload launch. General user-selected ports, cross-Runner
failover, private inter-Instance Bindings and production DNS/TLS publication
are not part of Step ③.

## Deferred

- Importing a limited Compose file into this typed form. It does not advance
  the Mail acceptance and follows a working group on staging.
- Moving a volume to another Runner, automatic failover, chunked checkpoints
  and scheduled backup.
- Sleep inhibition for foreground jobs and private inter-Instance Bindings.
- IPv6 and DNS-scoped egress, direct MX delivery, general user-selected public
  ports, and production DNS/TLS publication.
