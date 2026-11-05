# Portable Native Process Service Group

Status: Draft  
Date: 2026-11-05  
Tracking: ato-run/ato#1690 (Phase D)

## Decision

One Derivative (D) of a portable Application may realize its Surface with two
or more cooperating native processes on one host. This is an **evaluator
capability of `ato.process@1`**, not a new Application kind, not a Kernel or
Core primitive, and not a wire format.

The group is expressed with objects that already exist, plus one additive and
omitted-when-empty `BoundStep` field:

| Concept | Bound representation |
|---|---|
| service | one `BoundStep` with `protocol = "ato.process@1"`, `op = "serve"` |
| service Port | one `BoundPort` with `from = <step id>` |
| service runtime | `BoundStep.runtimes["python"]` |
| service Binding | `BoundStep.bindings` naming an Application Binding |
| service state | `BoundStep.state` naming a `BoundDerivation.state` slot |
| declared peer edge | `BoundStep.peer_ports`, a list of Port ids served by earlier steps |

`BoundStep` gains `peer_ports: Vec<String>` with
`#[serde(default, skip_serializing_if = "Vec::is_empty")]`. Every Derivation
formed before this contract omits the field, so its canonical JSON bytes,
`DerivationRef`, and the `ContractRef` it runs under are unchanged. A
regression test pins an existing Derivation's exact canonical bytes and ref.

`[[derivation.service]]` in `ato.capsule/2` is authoring shorthand for a
process group. It compiles to the objects above and to nothing else. The
authoring-only kind `PortableDerivationKindV2::ProcessServiceGroup` is a
compiler classification; it is not part of D, K, or the wire.

## Authoring

```toml
[[derivation]]
id = "native-web"
use = "ato.process@1"

[[derivation.service]]
id = "backend"
argv = ["python3", "backend.py"]
cwd = "."
runtimes = { python = "3.12" }
ports = [{ id = "backend.http", guest_port = 8081 }]

[[derivation.service]]
id = "web"
argv = ["python3", "proxy.py"]
cwd = "."
runtimes = { python = "3.12" }
ports = [{ id = "app.http", guest_port = 8080 }]
connect = ["backend.http"]
```

A `service` entry declares its own `argv`, `cwd`, `env`, `runtimes`, `ports`,
`state`, `bindings`, and `connect`. Route-level `argv`, `cwd`, `env`,
`guest_port`, and `runtimes` are refused: every process fact belongs to the
service that uses it. `depends_on` keeps its existing meaning — authored
ordering of startup only — and is not a connection declaration.

`connect` names Ports served by other services of the same route. The authoring
compiler validates that each reference is declared, unique, served by a
*different* service that appears earlier in the compiled step sequence, and
that no `connect` entry is used on an `ato.oci@1` service group. The compiled
`BoundStep.peer_ports` keeps authored order, deduplicated.

## Explicit Port binding: no inference from argv

A native process group never guesses a Port from argv. For every Port a
service serves, the Runtime allocates a loopback host port and exports:

```text
ATO_ENDPOINT_<SANITIZED_PORT_ID>_PORT = <host port>
```

`backend.http` becomes `ATO_ENDPOINT_BACKEND_HTTP_PORT`, `app.http` becomes
`ATO_ENDPOINT_APP_HTTP_PORT`. The workload must bind that value. The Runtime
does not rewrite any argv element. The existing single-process route keeps its
historical behaviour (it replaces one exact occurrence of the declared guest
port in argv) because changing it would change what an existing D does; the
new multi-service route has no such rewrite, and an application that hardcodes
a guest port fails at bind time inside the sandbox instead of silently
receiving a different port.

For each declared peer Port, the consumer service receives the same variable
name, but the value is the loopback port of that edge's shim (see below). The
consumer therefore has a stable, explicit way to reach exactly the peers its D
declares, and no way to name an undeclared Port.

## Shared supervisor

One supervisor owns every process and helper of the selected route:

- services start in compiled order, one at a time; a service starts only after
  the previous service accepts TCP on its first declared Port;
- readiness polls the Port's allocated host port and fails if any started
  service exits first;
- the group exports one endpoint per declared Port for K observation (the
  Surface Port and any internal Port an observation names);
- if any service exits, the whole group is stopped (there is no independent
  restart and no second attempt inside one Run);
- stop terminates in reverse start order with a bounded wait, force-kills the
  process group on timeout, and only reports success when every process and
  helper has been reaped;
- owned runtime directories are removed after the processes are gone.

Failure to start any service stops and reaps what already started before the
Run fails. A start, readiness, exit, and stop outcome are distinct errors; the
Runtime does not collapse them into one generic launch failure.

## macOS peer transport: runtime-owned broker plus loopback shim

Seatbelt filters network operations by port, not by address: an SBPL rule can
allow or deny `tcp "*:<port>"` but cannot say "only 127.0.0.1". Allocating a
peer Port and granting outbound to that port would therefore also grant
outbound to any host on that port number. The group uses an explicit broker
instead:

- the Runtime opens a Unix socket under its own runtime root
  (`<runtime_root>/peer/broker.sock`, owner-only) and keeps it outside every
  workload sandbox;
- the broker holds a table `token -> 127.0.0.1:<real peer host port>`; a
  connection presents `ATO-PEER/1 <token>\n` and the broker relays bytes only
  to that target, then closes unknown or malformed requests;
- for every declared edge the Runtime starts one small shim in its own
  sandbox. The shim listens on a loopback port, accepts the consumer's TCP
  connection, connects to the broker socket, presents the edge token, and
  relays bytes. The consumer's env var for that peer names the shim port;
- the consumer's sandbox allows TCP connect only to its shim ports and TCP
  bind only to its own Ports. It never receives the real peer Port, and its
  outbound rules do not name any external destination.

The shim is runtime-owned code: its argv, target, and token come from the
Runtime, never from the bundle or the workload. The token is not exported to
the consumer's environment. A consumer that knows the shim port still reaches
only the token's target. The shim's argv is nevertheless visible to another
process of the same user on the same host; this is not a cross-user
confidentiality boundary, and the broker tokens are per-Run and short-lived.

## Linux

The intended transport on Linux is the existing scoped network path
(per-service network namespace plus Runner-owned ingress), which already backs
the hosted process executor. That path is a property of the hosted
`ResolvedRuntimeLaunchContext`; wiring it into the local portable Run is a
separate change and is **not** part of this increment. Until then the local
portable process group uses the same broker and shim as macOS. The Landlock
policy still restricts TCP bind to the service's own allocated Ports and TCP
connect to its shim Ports, so undeclared peer and external connections are
refused by the kernel on both platforms. The netns-based Linux realization
remains the target and is listed as unverified in the PR.

## Validator split

Group validation is split into:

1. **Common structural validation** — Surface shape and initial path, input
   closure, service count and Port count bounds, unique DNS-label service ids,
   every service serving at least one Port, exactly one HTTP Surface Port, all
   other Ports payload-opaque `ato.tcp@1`, per-service argv/cwd/env shape, and
   the visibility rule that every declared state slot and Binding has exactly
   one reader.
2. **Adapter validation** — what a particular step protocol can actually
   realize. The OCI adapter keeps its existing profile (shared
   `oci.platform`, digest-pinned images, resource budgets, workspace mount,
   entrypoint, component inputs) with unchanged behaviour and unchanged
   `DerivationRef` for existing groups. The process adapter validates its own
   profile: every step `ato.process@1`/`serve`, per-step Python runtime,
   non-empty argv, workspace-relative cwd, and the `peer_ports` graph
   described below.

The refactor moves checks; it does not relax the OCI group. Existing OCI
group tests and canonical bytes are the acceptance gate for the split.

## Profile (v0)

A native process group is admitted only when:

- the Application uses `ato.application/2` with one Surface, one
  `ato.workspace@1` input at `.`, and `effects = "pure"` unless a declared
  `ato.tcp-egress@1` Binding requires confirmation;
- it has 2–4 steps, each `ato.process@1`/`serve`, each id a unique RFC 1123
  DNS label;
- each step declares exactly its own `runtimes["python"]`, a non-empty argv,
  and a workspace-relative cwd; it declares no other runtime fact;
- the route declares 1–8 Ports; exactly one is the `app.http` Surface Port
  served by `ato.http@1`; every other Port is `ato.tcp@1`; each step serves at
  least one Port;
- each declared Binding is visible to exactly one service;
- every `peer_ports` entry names a Port served by a different, earlier step;
- there is no workspace build, no compiler, no filesystem state slot, no Model
  Set input, and no `ato.tcp-egress@1` Binding (each is refused explicitly
  rather than silently ignored).

Host networking, a Docker socket, privilege escalation, host mounts, and
arbitrary egress have no field in the model and cannot be expressed.

## Relationship to the OCI service group

`ato.oci@1` and `ato.process@1` are alternative evaluator capabilities over
the same Contract. A multi-container route and a multi-process route are two
Derivations of one Application, never a fallback for each other:

- the OCI group keeps its existing objects, admission, launch, stop, and
  `DerivationRef` computation unchanged;
- the process group adds no field to the OCI path and no field to a route with
  one serving step;
- runtime selection remains explicit. A host that cannot run the selected
  realization refuses it; it does not switch to the other realization, and no
  planner chooses one;
- K observes HTTP responses on logical Ports. It does not observe process
  identity, service topology, or internal-Port reachability, and this contract
  does not add such observations. Satisfying the same K with a different D is
  correct by definition and is not evidence that the service architecture is
  the same.

## Identity

- K, `ContractRef`, `ComputationRef`, wire versions, bundle digests, and the
  receipt schema are unchanged.
- The new optional `BoundStep.peer_ports` is omitted from canonical JSON when
  empty. Existing D bytes and `DerivationRef` are unchanged (regression test).
- Only the new route's own D includes `peer_ports`; changing a connection
  declaration changes that D, never K.
- PIDs, allocated ports, shim ports, broker tokens, socket paths, and
  supervisor outcomes are Run evidence, never inputs to an identity.

## Decisions

1. **Additive `BoundStep.peer_ports`, not a new semantic object.** The edge
   exists only so the Runtime can grant exactly what D declared. Creating a
   `Service`/`Connection` object in the Semantic Core would add a workload
   graph that the accepted Core Model deliberately does not have, and encoding
   it in `env` would invent an inferred convention. An omitted-when-empty
   field on a bound step keeps old bytes intact.
2. **A new authoring kind, not a reinterpretation of `Process`.** The compiler
   must know whether `argv`/`guest_port` are route-level or per-service. A
   distinct authoring-only kind keeps the single-process grammar and its
   historical argv rewrite exactly as they were.
3. **No argv inference for the new path.** Port guessing only works for one
   process and one Port; with several processes it can silently connect
   services to the wrong port. Explicit environment binding is the only
   unambiguous rule, and it matches the existing process execution ABI.
4. **A gated supervisor, not per-service lifecycles.** Services of one route
   are one Application trust boundary. Independent restart or partial survival
   would make K satisfaction depend on which component survived.
5. **Broker plus shim on macOS.** SBPL cannot express "loopback only", so
   port-scoped outbound would grant the same port number anywhere. A
   runtime-owned broker makes the declared edge the unit of authority, and the
   shim keeps the workload interface an ordinary TCP connection.
6. **Linux reuses the same broker in this increment.** The scoped network path
   belongs to the hosted executor context; reusing it locally needs a larger
   adapter. Landlock per-port rules already enforce the same allowlist, and
   pretending the netns path is wired would be dishonest. This is recorded as
   the main unverified/deferred item.
7. **No topology observation in K.** Adding a topology observation would be a
   new Contract verifier and a new semantic noun. The honest statement is that
   K observes declared responses only.

## Status of this increment

Implemented in this increment:

- the `ato.capsule/2` process service grammar (`service` under a
  `ato.process@1` derivation) and its `connect` edges;
- the additive `BoundStep.peer_ports` field, omitted from every route that
  does not use it;
- the shared structural validator plus separate `ato.oci@1` and
  `ato.process@1` adapter validation (OCI behaviour and identity unchanged);
- the one supervisor (ordered start, per-service readiness, group-wide stop,
  reaping) and its unit tests;
- the runtime-owned Unix-socket broker and loopback shim, with unit and
  integration-level tests of relay and authorization;
- a two-service fixture (`samples/native-process-group`) whose K includes an
  undeclared-egress observation, and a CLI acceptance test.

Not verified in this increment, and not claimed:

- the CLI end-to-end Run of the fixture on the development host: that host
  already applies a Seatbelt sandbox to the test process, and a nested
  `sandbox_init` is refused (`Operation not permitted`). The tests report this
  as skipped, never as passed;
- any execution on Linux or on a native Linux Docker host;
- the network-namespace realization described above;
- behaviour under host reboot, sleep/wake, or a killed supervisor.

## Not in this contract

- Importing Compose files, includes, or interpolation.
- Parallel start, independent restart, health-based supervision, or
  cross-service DNS.
- Cross-Runner placement, fixed public Ports, or private inter-Instance
  Bindings.
- Linux network-namespace realization of the local portable lane.
- Any change to OCI group admission, launch, stop, or identity.
