# Contract Verifier Registry

Status: Draft  
Date: 2026-08-23

## Decision

A Contract is an extension-defined assertion that a physical candidate
Realization is acceptable for an already-known target `ComputationRef`.
Contracts neither create nor alter Computation identity.

```text
Materializer.restore (hidden candidate; replay/restore complete)
  -> Realization.activate (candidate-internal endpoints only)
  -> ContractVerifierRegistry.resolve(verifier_id)
  -> ContractVerifier.verify(payload)
  -> Realization.publish (external Surface)
```

`ContractDescriptor` contains a versioned `verifier_id` and opaque JSON
`payload`. Core does not define a Contract enum and Player does not interpret
Contract payloads. A product assembly explicitly registers verifier
extensions. Missing verifiers fail closed.

## Lifecycle and cleanup

`Materializer.restore` returns an unpublished candidate. `activate` may start
only the internal endpoints required by verifiers. `publish` is a separate
lifecycle transition and may run only after all Contracts pass.

Activation, verification, and publication failures all call `quiesce` before
the error is returned. An accepted candidate owns a cleanup-on-drop guard, and
normal `wait` completion is also followed by `quiesce`. A cleanup error is
preserved together with the original failure.

Replay completion and Contract acceptance are therefore distinct outcomes:

```text
Record apply completed != Candidate Realization accepted
```

## Descriptor carriage

`ato.replay@2` carries canonical `ContractDescriptor` values and exposes them
through the generic Materializer interface. `ato.replay@1` retains its legacy
wire shape and does not silently adopt the v2 Contract meaning.

## Initial verifier extensions

- `ato.contract.http@1`: loopback-only HTTP status and optional body
  `ContentRef` verification with bounded time and response size.
- `ato.contract.workspace@1`: normalized workspace-relative file verification
  against a `ContentRef`, with boundary and size checks.

These are ordinary registered extensions. Their payload types do not appear
in Player, Computation, or the Contract registry API.

## Identity boundary

Contract descriptors and results are acceptance metadata. Neither is used to
derive `ComputationRef`. Materialization descriptors may include Contracts
without changing the target logical identity.

## Source-bound vs behavioral Contract identity

`ato.capsule/1` authors may state what their Contract's identity depends on:

```toml
[contract]
identity = "behavioral"   # or "source-bound"; omit to bind as authored
```

| identity | requirement set | `K` |
|---|---|---|
| omitted | anything authored | bound exactly as authored (behaviour unchanged) |
| `behavioral` | must not observe `ato.contract.workspace@1` | the same `ContractRef` for any source that behaves the same way |
| `source-bound` | must observe exactly one workspace input, with a resolved digest | the `ContractRef` names that source closure |

This is authoring intent, not a Contract field: `BoundContract` keeps its
`{schema, requirements}` shape, so stating an identity adds no bytes to `K`.
An explicit identity that contradicts the requirements is refused at bind
(`behavioral_contract_has_source_requirement`,
`source_bound_contract_has_no_source_requirement`) rather than having the
binder rewrite `K`. Preset synthesis states `source-bound`; a Formation job may
request `behavioral`, and an authored manifest that states its own identity
outranks the request. `ato.capsule/2` accepts `behavioral` and refuses
`source-bound`, because the portable authored grammar cannot bind a source
identity.

## Supersession

For v2 Materializations this RFC supersedes any wording that treats replay
completion as sufficient to publish a Run. Legacy v1 readers remain available.
