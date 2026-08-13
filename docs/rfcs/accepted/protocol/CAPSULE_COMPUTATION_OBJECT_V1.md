---
title: "Capsule Computation Object Encoding v1"
status: accepted
date: 2026-08-13
author: "@egamikohsuke"
ssot:
  - "crates/capsule-core-codec/"
related:
  - "CAPSULE_PROTOCOL_SEMANTIC_CORE.md"
---

# Capsule Computation Object Encoding v1

## 1. Identity preimage

The sole v1 preimage of a `ComputationRef` is the RFC 8785 JCS encoding of this
JSON value:

```json
{
  "semantics": "<SemanticsId>",
  "boundary": {
    "<PortId>": {
      "protocol": "<ProtocolId>",
      "role": "<RoleId>"
    }
  },
  "residual": "<ContentRef>"
}
```

No field is optional. Unknown fields are invalid. The boundary is a JSON
object keyed by unique `PortId`; JCS determines encoded member order. All IDs
and references must pass their Core parsers before the value is accepted.

## 2. Reference derivation

```text
canonical_bytes = JCS(ComputationObject)
digest          = BLAKE3-256(canonical_bytes)
ComputationRef  = "blake3:" + lowercase_hex(digest)
```

Decoders must reject inputs larger than 1 MiB before parsing, re-encode the
parsed value, and require byte equality with the input. Valid but non-canonical
JSON is rejected. `ComputationRef` parsing checks syntax only and does not
resolve an object. A `ResolvedComputation` may be constructed only after
canonical decoding and an exact digest comparison with the supplied reference.

## 3. Resolution boundary

Storage is abstracted by an `ObjectResolver` that exposes metadata and a byte
stream for any `ContentRef`. Computation resolution is derived by opening the
referenced bytes, enforcing the bounded object size, checking the observed
size, applying this codec, and verifying the digest. Future semantics consumers
may reuse this resolver boundary so residual and transitive objects do not
require a physical CAS path convention.

## 4. Stability

The field set, JCS representation, identifier spelling, and hash algorithm are
identity-bearing. `ComputationRef` as defined by the current Core is permanently
bound to this Computation Object v1 encoding. An incompatible future encoding
must use a distinct reference domain and type; it must not reinterpret an
existing `ComputationRef` or silently alter v1 output. Resolution of the
current type therefore always uses this codec and requires no in-band version
field. Cross-implementation golden vectors live under
`crates/capsule-core-codec/tests/vectors/`.
