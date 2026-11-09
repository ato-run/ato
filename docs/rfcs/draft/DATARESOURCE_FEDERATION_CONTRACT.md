# DataResource Federation Contract

Status: **Draft / normative for the cross-surface implementation (G0-G7)**.
This document does not claim that every listed capability is implemented or
deployed; each surface reports its own status against the conformance matrix.

It fixes the contract shared by `ato` (OSS core, selfhost Coordinator, CLI,
Desktop), `ato-api` (Hosted D1), `ato-pwa`, and the TODO/Calendar applications.
Hosted-only implementation notes remain in `ato-api/docs/rfcs/draft/` and are
not restated here.

## 1. Scope and non-goals

- A DataResource is the mutable authoritative store for application data. It is
  **not** a Capsule and never becomes a new semantic primitive.
- One authority per Resource: the user-selected Home Coordinator (Hosted D1 or a
  selfhost SQLite Coordinator). There is never a second writable authority.
- Out of scope for this contract: organization ownership/delegation, bulk MFA
  authority transfer, complete backup/restore and key recovery, cross-Coordinator
  ACID, offline multi-master editing/merge, non-JSON backends, billing, and
  production rollout.

## 2. Concepts that stay distinct

| Concept | Meaning | Identity? |
|---|---|---|
| `ResourceId` | User-owned, mutable logical reference; independent of app, Instance and Capsule | data identity; never changes on move/copy |
| head `Revision` | CAS unit and history position in the authority | version of state |
| Continuation `C_r` | current state plus the Protocol operations available from it | a point, not a hash |
| Protocol Operation | named operation defined by an app-owned, versioned Protocol | operation meaning |
| Evolution / Observation | Protocol-classified state transition / read | — |
| `Record` / `Receipt` | record of an observed transition / evidence of a run | evidence; not identity |
| `seal` | fixes the acceptance conditions K for a Continuation | — |
| `CapsuleId` | wire v4: `root_contract_ref = ContractRef(K)` | Capsule identity |
| `DerivationRef` | canonical reference of one realization path D (outside K) | D identity |
| `bundle_sha256` | whole-file transport equality | **not** identity |
| Materialization | replay/filesystem/snapshot/OCI/VM strategy | implementation detail |

A Revision update is not a seal. A Receipt is not a Record. The same CapsuleId
does not imply the same Resource, revision or data.

## 3. Identity and wire rules

- wire v4 `ato.portable-application/2`: `CapsuleId = ContractRef(K)`. v4 admits
  `index.instance_snapshot_ref`; a snapshot-bearing K must contain exactly one
  requirement with verifier `ato.contract.instance-snapshot@1` whose digest
  equals that reference. Re-exporting changed saved state forms a new `K_saved`
  while D is unchanged.
- wire v3 `ato.portable-application/1` identifies with `ContractRef` and rejects
  snapshots/portability manifests. wire v2 is rooted at `ComputationRef`.
  References are never cast or renamed across profiles, and v4 is never
  downgraded to v3.
- `bundle_sha256` is transport evidence only. A validated bundle is not
  evidence that its snapshot was restored; restore evidence comes from the
  runtime observation bound to the attempt receipt.

## 4. Authorization invariants

Every entry point (PWA, app UI, MCP, CLI, Desktop, consumer Port) evaluates the
same intersection before acting:

```
principal ∩ origin policy ∩ Binding ∩ Port.required_operations ∩ active Grant chain
```

1. An operations-scoped Grant carries `scope_kind = operations` and may only
   invoke its listed operations. It never grants document read, replace or
   export through any first-party or application API.
2. Every active ancestor of a Grant must be operations-scoped and list the same
   operation, so a child Grant can never be broader than its parent.
3. An empty operation list means an operations Grant that allows nothing;
   omitting the list means a document Grant. The two are never conflated.
4. Protocol operations are published once per (protocol, operation) by a
   verified publisher (owning account and app) with an immutable handler digest.
   A conflicting claim, including a concurrent one, is refused. A Resource pins
   the registered definition set; a changed digest fails closed.
5. Revocation and downgrade are evaluated at every read of stored results: a
   caller that can no longer invoke an operation cannot read its past receipt.
6. Every entry point applies the same account/permission/exposure gate before
   any handler is selected or executed, and unknown or unverifiable operations
   fail closed.

## 5. Commit invariants

- A write is authorized and committed atomically with `expected_revision`, the
  actor/grant/binding/schema fence and the `operation_id`.
- The commit, its Receipt and the Protocol operation result are written in one
  database transaction: after a crash, the same key returns the stored result
  and never re-executes the trusted handler.
- Observations do not advance the head revision and return only the fields the
  Protocol declares.
- A second authority is never created implicitly. Moving the authority keeps the
  `ResourceId` and fences the old authority with an epoch; copying data with a
  new ID is an explicit export/import, not authority transfer.
- Losing the network to the authority fails closed. There is no silent merge,
  no cached write and no fallback authority.

## 6. Operation envelope

```
invoke:  operation_ref | (port, operation), operation_id, expected_revision, arguments
result:  status succeeded|failed|conflict|revoked, persisted, result, revision, receipt
```

Same `operation_id` + same payload returns the original outcome; a different
payload is a conflict; an expired key is not re-executed. Observation results
contain only the declared fields (for Calendar: `task_id`, `title`, `due_at`,
`completed`).

## 7. ProtocolSemantics interface

```ts
interface ProtocolSemantics {
  protocolRef: string;
  allowedOperations(): ReadonlySet<string>;
  classify(op: string): "evolution" | "observation";
  validateInput(op: string, input: unknown): ValidationResult;
  validateOutput(op: string, output: unknown): ValidationResult;
  resultFields(op: string): readonly string[];
  receiptPolicy(op: string): { record: boolean; retentionDays: number };
  handlerDigest(op: string): string;
}
```

Business meaning lives in the application-owned Protocol (TODO owns
`todo.board@1`); the Kernel never knows TODO CRUD. `ato.data.json@1` remains the
storage form.

## 8. Conformance matrix

Hosted D1 and the selfhost SQLite Coordinator must pass the same cases:

| Area | Case |
|---|---|
| Authority | selected Coordinator is the only authority; crash/restart loses no committed revision |
| CAS | concurrent commit: exactly one winner; expected_revision honoured |
| Idempotency | same operation_id/payload -> same result; different payload -> conflict; lost response recovers result |
| Grants | operations Grant cannot reach document read/replace/export; ancestor intersection; revoke blocks immediately |
| Publisher | foreign publisher, conflicting digest, concurrent claim refused; pinned handler mismatch fails closed |
| Consent | account MCP level, connection capability and expose.mcp apply on every surface; downgrade hides past receipts |
| Quota | storage full / retention / GC observable; no partial commit |
| Transport | relay opaque, offline fail-closed, origin_epoch fencing, capability negotiation for unverifiable encryption modes |
| Surfaces | PWA, app UI, MCP, CLI, Desktop observe the same revision/receipt/revocation decision |

A surface is not conformant because its fixtures pass: the acceptance runs the
real sample applications (shared-json-todo -> shared-json-calendar) against a
real authority.

## 9. References

- `docs/rfcs/draft/K_D_MODEL.md` (K/D relations, wire v4 profile)
- `docs/rfcs/draft/PORTABLE_INSTANCE_SNAPSHOT.md` (saved-state closure)
- `docs/capsule.md`, `docs/concepts/application-contracts.md` (profile identity)
- ato-api: `docs/rfcs/draft/data-resources.md`, `data-resource-operation-scope.md`
  (Hosted D1 implementation and its tests)
- Source material: workspace `docs/rfcs/draft/data-resource-continuation-contract.md`
  and the DataResource Federation RFC (Grill-me 72 questions, v4)
- PRs: ato-run/ato-api #1155, ato-run/ato #1726
