# ADR-043 — External endpoint Bindings for Contract observations

Status: Draft. Design only — nothing here is implemented or deployed.
Designs the gap recorded in the self-hosted mail plan (PR ato-run/ato#1665,
"Detected gap before S3: K3's 443 is served by the host") and depends on S1
(ato-run/ato#1666) being merged. No Kernel, Capsule-format or mail-specific
feature is added.

## Problem

A Contract observation (`ato.contract.http@1`, `ato.contract.tls@1`) names a
**logical Port of the candidate**, and the runtime resolves that Port to the
endpoint it actually realized (`required_*_observations` in
`lib/runtime-attempt/src/verification.rs` and `validator_agent.rs`). Some
services a Capsule depends on are by design not realized by the candidate: in
the mail D1 the host Caddy terminates 443 and owns the public certificate,
while the app only consumes the certificate as a secret Binding. K3 ("the HTTPS
admin / JMAP endpoint answers") and K5 for 443 therefore name a target the Run
cannot currently observe, so a `fully_satisfied` receipt cannot cover 443.

Requirements (product owner):

1. K must not contain an IP address, Runner or Caddy identity.
2. The Derivation D declares the dependency on the external service as a
   Binding.
3. The control plane resolves the actual endpoint from an **authorized**
   Binding.
4. Verification reuses `ato.contract.tls@1` unchanged in syntax and meaning.
5. An endpoint that cannot be resolved fails closed (unobserved), never
   assumed.
6. Until the 443 observation succeeds, the mail Capsule is not
   `fully_satisfied`.
7. No mail-specific Kernel or Capsule-format feature.

## Audit basis (two states, named separately)

- **`origin/stg` `e2e6c3386`**: logical Ports and Bindings, the operator-managed
  `ato.tcp-egress@1` path, fixed-TCP allocations and bindings, the runner
  ingress allowlist, and the hosted verification/ack flow exist.
- **S1 branch (unmerged)**: `ato.contract.tls@1` and receipt `/3` exist only in
  ato `d6f85395b..a318e6473` (#1666); `tcp_endpoints` and `/3` acceptance exist
  only in ato-api `9e076cf6` (#1079). **This design depends on both being
  merged.**

What exists (verified against the code):

- **Ports are logical; Bindings map them to physical endpoints**
  (`BoundPort {id, protocol, from, guest_port, client_address_transport}` in
  `lib/formation/src/authoring.rs`). `BoundPort.from` names the serving step;
  six sites assume `from == step id`.
- **Bindings are declarations with protocol and required flag**
  (`ApplicationBindingV1`), injected per step as environment
  (`BoundStep.bindings`; `resolve_application_bindings` builds the whole
  application env map) and delivered as a one-time sealed grant (ato-api
  `capsule_binding_grant.ts`, `runner_leases.ts` `/:id/runtime-bindings`).
- **Operator-managed Bindings already exist**: `ato.tcp-egress@1` cannot be
  supplied by the owner (`tcp_egress_binding_is_operator_managed`) and is
  resolved from an operator-created grant.
- **Fixed TCP fencing**: `instance_tcp_allocations` + `runtime_tcp_bindings`
  fence an address to a Run **and lease and generation**; the hosted
  verification job names only the Run's current-lease, allocated endpoints.
- **SSRF-safe operator registries already exist**: `runner_ingress` /
  `runner_ingress_slots` (operator-registered public origins) and the
  `isPublicRoutableIpv4` predicate used by their admin path.
- **Runtime verification**: the Rust validator probes HTTP through the API
  relay and TLS only on control-plane-named `tcp_endpoints`; receipt `/3`
  records the presented certificate facts; the validator validates the receipt
  with `accept_receipt` before reporting it.

Missing pieces, and nothing else: a Binding protocol for an **observation-only
external endpoint**, an authoring place for a **Port that no step serves but a
Binding maps**, a **control plane resolution record** with revision and lease
fencing, and **binding identity in the receipt evidence**.

## Decision (minimum generalization)

### 1. A new operator-managed Binding protocol

```text
ato.endpoint.tcp@1
```

An observation-only L4 endpoint, declared by the Derivation:

```toml
[[binding]]
id = "edge_https"
protocol = "ato.endpoint.tcp@1"
required = true
```

- Its value is **never authored and never owner-supplied**. Like
  `ato.tcp-egress@1`, the control plane resolves it from an operator-created,
  owner/environment/Instance-scoped record.
- It is **not injected** into the workload environment; there is no consumer.
  It is consumed by verification only.
- It names no application protocol: TLS/HTTP semantics stay with the
  `ato.contract.*` verifier that observes the Port.

### 2. A binding-backed Port (the authoring addition this needs)

`BoundPort` gains one optional field, absent from canonical bytes when unset
(so every existing `DerivationRef` is unchanged):

```rust
pub struct BoundPort {
    ...
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding: Option<String>,   // set only for binding-backed Ports
}
```

`from` keeps its meaning (the serving step). A binding-backed Port has
`from = ""` (or an explicit "no step" sentinel decided in implementation) and
`binding = Some(<Binding id>)`; validation enforces exactly one source.
The Port protocol stays the existing `ato.tcp@1`; there is no new Port
protocol and no protocol-dependent meaning for `from`.

**Authoring grammar**: neither v1 nor v2 has a place for this today. v1 has no
`[[binding]]` at all; v2 declares Ports per service (`[[derivation.service.ports]]`,
no `protocol`/`from`) or one `guest_port`. The minimum addition is a
derivation-level Port table in v2 that is legal only for binding-backed Ports:

```toml
# derivation-level, binding-backed only (D1 is a service group)
[[derivation.port]]
id = "edge.https"
binding = "edge_https"
```

Served Ports keep their current form; the new table cannot serve a step or a
service. This is an authoring-grammar change only; the Capsule format and
`BoundPort` identity are extended by one optional field.

K is unchanged:

```toml
[[contract.observation]]
id = "edge-https-cert"
use = "ato.contract.tls@1"
port = "edge.https"
server_name = "mail.ato.run"
start = "implicit"
```

Nothing in K names Caddy, an IP, a Runner, an address or a binding id.

### 3. Authorized endpoint resolution (control plane, not D, not the owner)

New operator registry (a new table; the ingress-slot registry is a per-runner
vhost map, not an observation-target allowlist):

```text
endpoint_bindings
  id            ULID
  account_id    owner scope
  environment   staging | production
  instance_id   the Instance the binding is lent to
  protocol      ato.endpoint.tcp@1  (one value today)
  address       literal ip:port, public unicast only
  revision      monotonic, bumped on any change
  status        active | revoked
  created_by, created_at, revoked_at
```

Validation at registration and use (the same rule set the ingress admin path
uses): parse `address` as an `IpAddr` literal (never a hostname), refuse
private, loopback, link-local, multicast, CGNAT and unspecified addresses
(`isPublicRoutableIpv4` today; IPv6 needs the equivalent predicate before
IPv6 is allowed), and refuse a port outside the operator-declared one.

At **lease authorization** the API validates the declaration against the
registry (account, environment, Instance, protocol, status, address) and
persists a per-Run authorization row, mirroring `runtime_tcp_bindings`:

```text
runtime_endpoint_bindings
  binding_id, run_id, lease_id, revision, address, status(pending|active|detached|revoked)
  unique (binding_id, run_id) — not exclusive across Runs
```

Rules:

- A changed or revoked registry row bumps/strands the revision; an existing
  Run's authorization is not rewritten. The running Run keeps the endpoint it
  was authorized with; a new revision requires an owner re-run (same decision
  as the certificate Binding in the plan's G2). Revocation detaches the
  authorization.
- Several Runs may hold the same binding; the per-Run row, not an exclusive
  lock, is the fence.
- The verification job names only `endpoint_bindings` rows whose run, current
  lease, status and revision match the persisted authorization, next to the
  existing `tcp_endpoints`:

```json
"endpoint_bindings": [
  { "binding_id": "edge_https", "revision": 7,
    "protocol": "ato.endpoint.tcp@1", "address": "203.0.113.10:443" }
]
```

- **Compatibility**: `ValidationJob` is `#[serde(deny_unknown_fields)]`, and
  the ack evidence schema is strict. Adding `endpoint_bindings` to the job (and
  `binding` to `evidence.tls`) must therefore ship in one coordinated change:
  the validator first accepts the optional field (no API emits it yet), then
  the API starts emitting. The reverse order breaks every hosted job. If that
  coordination is not possible in one window, gate the field on a job schema
  version instead.

### 4. Runtime verification

- The Rust resolver maps a TLS requirement's Port to its endpoint source: a
  served Port uses `tcp_endpoints` as today; a binding-backed Port uses
  `endpoint_bindings` looked up by `BoundPort.binding`.
- It validates protocol, a nonzero revision, run/lease identity against the
  job's own ids, and parses the address as a literal `SocketAddr` (no DNS, no
  hostname).
- It then calls the same `ato.tls-observe` probe with the Port's
  `server_name`/`start`. No new probe, no new Contract grammar.
- Formation already defers every TLS requirement to the runtime
  (`ObservationOutcome::Deferred`, seal admits Deferred); sealing additionally
  validates that a binding-backed Port names a declared Binding and that no
  step references that Binding (it is not injected).
- CLI/local runs cannot resolve operator bindings. A binding-backed Port is
  therefore unobservable locally: `ato run` on such a bundle fails closed
  (not-observable), which is stated here rather than worked around with an
  arbitrary address option.

### 5. Failure classification (requirement 5)

| Situation | Behaviour |
|---|---|
| No authorization row / binding inactive or revoked / revision or lease mismatch | No observation -> `tls_observation_missing` or `tls_observation_mismatch`; receipt is `fully_satisfied = false` |
| Address invalid at claim time (private, unparsable) | The API never emits it; a defence-in-depth refusal in the validator is the same unobserved outcome |
| Connection refused / unreachable / timeout | Observation attempt aborted: hosted reports `observation_unreachable` and the attempt is retried/abandoned; it is not the app's Contract failure |
| Certificate presented but wrong chain/name/validity/handshake incomplete | Facts recorded, verdict failed, receipt `/3`, `fully_satisfied = false` |

The ack already requires the receipt's observation id set to equal the
validator report's `requirement_ids`; a receipt that omits the 443 requirement
cannot verify, which is the mechanism behind requirement 6.

### 6. Receipt evidence and authority split

A satisfied TLS verdict for a binding-backed Port records the Binding it
observed, in the `/3` evidence object:

```json
"tls": { "...": "...",
  "binding": { "id": "edge_https", "revision": 7,
               "protocol": "ato.endpoint.tcp@1" } }
```

- Optional and absent for served Ports, so existing /3 evidence is unchanged.
- **/3 is unshipped S1 work, so the field is added inside /3 now**; no /4 is
  needed unless a /3 receipt ships before this lands.
- Rust `accept_receipt` requires the `binding` exactly for binding-backed
  Ports (max K/D authority). The API **must** additionally compare the
  reported `binding.id`/`revision` with the persisted Run authorization and
  reject a mismatch; it must not re-interpret D or K in TypeScript. "Is this
  Port binding-backed" is answered from the Run authorization, not by parsing
  the bundle in TS.

## Security analysis

- **SSRF**: no author- or owner-controlled string reaches the connector. The
  address is an operator-typed **literal IP** in an allowlisted registry,
  validated by the public-routable predicate at registration and again by
  literal parse at observation. Hostnames are refused at both ends: a hostname
  would be re-resolved at observation time (`to_socket_addrs`) and could reach
  a private target after a passing registration check.
- **Stale reuse**: run + current lease + persisted revision + registry status
  are checked at claim time; revocation detaches; a binding change requires an
  owner re-run.
- **Secret leakage**: endpoint bindings are observation-only, never injected
  into the workload environment; the secret surface is unchanged.
- **Audit**: receipt names binding id + revision; registry and per-Run
  authorization are durable records.

## Alternatives considered

- **Address/hostname in K**: rejected (violates requirement 1; same K would
  differ per environment).
- **Reuse `tcp_endpoints` for 443**: rejected. Those are candidate-owned
  listeners fenced to the Run; the host Caddy is not the candidate's, and a
  receipt could not say which authorized Binding was observed.
- **Extend `ato.tcp-egress@1`**: rejected. An egress grant authorizes a
  CIDR + ports for the workload's outbound side and pins no single address, so
  it cannot name an observation target.
- **Port protocol branching on `from`**: rejected. Six sites assume
  `from == step id`; an optional `binding` field keeps each meaning single.
- **Verify 443 by ops procedure only**: rejected (requirement 6).
- **Move Caddy into the app (plan D2)**: a later Derivation, not the general
  fix; it conflicts with the host Caddy owning 443 during D1.

## Decisions taken from review

1. New `endpoint_bindings` registry (not an extension of the ingress-slot
   vhost map).
2. Literal **IP only**; no hostname in the registry (Q2 was blocking).
3. Per-binding monotonic revision (not the Instance-wide portable binding
   config generation, which mixes unrelated changes).
4. v2 needs a new derivation-level Port table for binding-backed Ports;
   defining it is part of implementation, and it must not change any existing
   `DerivationRef`.
5. The `binding` evidence field ships inside `/3` (which is unshipped).
6. CLI/local behaviour is fail closed; no arbitrary-address override.

## Open questions for implementation review

1. Exact shape of the derivation-level Port table: one entry per binding-backed
   Port (`id` + `binding`) vs the Binding naming its Port. The former keeps
   Ports the single observation target; the latter is fewer lines but lets a
   Binding declare semantic surface.
2. Whether an authorized endpoint may be reused by two Instances of the same
   owner (proposed: yes, per-Run fence; scope is the Instance that declares it).
3. IPv6 support (needs the IPv6 public-routable predicate before it is
   accepted).

## Rollout

1. ato (validator): accept optional `endpoint_bindings` and optional
   `evidence.tls.binding`; no emission yet.
2. ato-api: accept the same, add registry + authorization + job field; start
   emitting and correlate on ack.
3. ato (binding-backed Port + resolver + receipt evidence), tests.
4. Mail: the D1 sample adds `edge.https` (Caddy) and only then can be
   `fully_satisfied`.
5. Staging proves it with a real Let's Encrypt certificate before production is
   considered.

Not in scope: changing the existing production container, DNS/MX, or the host
Caddy configuration.
