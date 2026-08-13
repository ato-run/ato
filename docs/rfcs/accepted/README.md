# Accepted RFCs

Accepted RFCs are the deeper reference layer that backs current implementation.
This index identifies which documents own each compatibility surface. A shared
word such as “Capsule” does not make an Ato implementation document part of the
Capsule Protocol.

## Capsule Protocol authority

The active normative Capsule Protocol consists of exactly three documents:

| Layer | Authority | Defines |
|---|---|---|
| Semantics | [Capsule Protocol Semantic Core](protocol/CAPSULE_PROTOCOL_SEMANTIC_CORE.md) | Computation + typed Ports; Protocol v1 compatibility semantics |
| Wire | [Capsule Protocol CBOR v1](protocol/CAPSULE_CBOR_V1.cddl) | descriptor and I/O Record bytes |
| Container | [Capsule Protocol Bundle v1](protocol/CAPSULE_BUNDLE_V1.md) | portable `.capsule` layout, closure, integrity, and limits |

Adapter and runtime extensions are intentionally outside this set. PTY payload
semantics, Ready-State formats, signing, trust, lineage, and fork policy become
separate specifications only when their interoperability surface requires one.

## Ato implementation authorities

The other accepted SPECs and ADRs in this directory describe Ato authoring,
resolution, build, runtime, isolation, CLI, and service behavior. They may use
manifests, execution contracts, snapshots, materializations, or bindings as
implementation strategies and derived views. Those concepts are not Capsule
Protocol Semantic Core primitives.

`CAPSULE_IPC_SPEC.md` defines Ato's JSON-RPC IPC and Broker behavior. It is not
the Capsule Protocol Semantic Core or its State + I/O Protocol v1 compatibility
representation. Its legacy filename is retained until that implementation
specification is separately renamed and re-audited.

## Archived Capsule documents

Two formerly accepted documents are no longer active authorities:

- [Capsule Artifact Format v2](../archived/CAPSULE_FORMAT_V2.md) defines a
  legacy Ato distribution artifact whose layout conflicts with Protocol Bundle
  v1.
- [Capsule v1 Execution Identity and Snapshot Model](../archived/CAPSULE_V1_EXECUTION_MODEL_SPEC.md)
  is superseded by the Semantic Core and remains only as an implementation-
  compatibility reference.

Archived documents MUST NOT be used to infer Protocol behavior.

## Maintenance rule

If an accepted RFC diverges from code, update the RFC or move it out of
`accepted/`. Topic pages under `docs/` are shorter entry points and do not
override the authorities listed here.
