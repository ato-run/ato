---
title: "Capsule Protocol Bundle v1"
status: accepted
date: 2026-08-12
author: "@egamikohsuke"
ssot:
  - "crates/capsule/src/protocol_bundle.rs"
related:
  - "CAPSULE_PROTOCOL_SEMANTIC_CORE.md"
  - "CAPSULE_CBOR_V1.cddl"
---

# Capsule Protocol Bundle v1

## 1. Authority

This specification defines Bundle v1, the portable single-file container for
Capsule Protocol State and I/O. It is the authority for the outer `.capsule`
layout produced and consumed by `PortableCapsule`.

The three normative Capsule Protocol layers are:

```text
CAPSULE_PROTOCOL_SEMANTIC_CORE.md  Computation semantics and v1 compatibility
CAPSULE_CBOR_V1.cddl               descriptor and record bytes
CAPSULE_BUNDLE_V1.md               portable single-file container
```

The archived Capsule Artifact Format v2 is a different legacy distribution
artifact and does not define this container.

## 2. Scope

Bundle v1 defines:

- the deterministic TAR container;
- required protocol members and content-addressed object members;
- reference closure and object integrity;
- aggregate resource limits; and
- fail-closed reader behavior.

State adapter formats, Connector payload semantics, signatures, trust,
lineage, and fork policy are outside this specification. In particular, an
object referenced by a State type may itself be a TAR or another structured
format, but that inner format belongs to the State adapter.

## 3. Container

A Bundle v1 file is an uncompressed deterministic TAR archive. Its conventional
file extension is `.capsule`.

Writers MUST encode every member as a regular file with a UTF-8, relative,
canonical path. Writers MUST NOT emit absolute paths, `.` or `..` components,
platform path prefixes, links, device entries, sparse entries, PAX extension
entries, or unknown members.

Writers MUST emit members in this order:

1. `protocol/descriptor.cbor`;
2. `protocol/records.cborseq`; and
3. zero or more object members, sorted by the bytewise ascending canonical
   `ContentRef` string `algorithm:digest`.

TAR headers MUST use these deterministic values:

| Field | Value |
|---|---:|
| entry type | regular file |
| mode | `0644` |
| uid | `0` |
| gid | `0` |
| mtime | `0` |

The standard TAR checksum and block padding are computed from those values and
the member bytes. Writers MUST finish the archive with the standard TAR end
blocks.

## 4. Members

### 4.1 Required protocol members

`protocol/descriptor.cbor` MUST occur exactly once and contain one Descriptor
v1 item conforming to `CAPSULE_CBOR_V1.cddl`.

`protocol/records.cborseq` MUST occur exactly once and contain an RFC 8742 CBOR
Sequence of zero or more I/O Record v1 items conforming to
`CAPSULE_CBOR_V1.cddl`. The member remains required when the sequence is empty.

### 4.2 Object members

An object member has exactly this path:

```text
objects/<algorithm>/<digest>
```

`<algorithm>:<digest>` MUST be a valid `ContentRef` under the CBOR v1 contract.
Bundle v1 therefore permits `blake3` and `sha256`, each followed by exactly 64
lowercase hexadecimal characters.

The member bytes are the referenced object bytes. A bundle MUST NOT contain the
same canonical `ContentRef` more than once. Additional unreferenced objects MAY
be present, but every stored object remains subject to integrity validation and
all bundle limits.

No other member path is defined in Bundle v1.

## 5. Closure and integrity

Before a bundle is accepted, a reader MUST enumerate every `ContentRef`
reachable from:

- `descriptor.base_state.state_ref`;
- every non-null `connector.config_ref`; and
- every object-backed I/O Record payload.

Every reachable reference MUST have a corresponding object member in the same
bundle. A reader MUST verify every stored object's bytes against the algorithm
and digest in its canonical `ContentRef`, including objects that are not
reachable from the descriptor or record stream.

Missing reachable objects and digest mismatches invalidate the entire bundle.
Validation MUST complete before replay or continuation begins.

## 6. Resource limits

Bundle v1 uses the following fail-closed limits:

| Resource | Maximum |
|---|---:|
| physical TAR file size | 1 GiB (`1,073,741,824` bytes) |
| one member | 512 MiB (`536,870,912` bytes) |
| aggregate unpadded member bytes | 1 GiB (`1,073,741,824` bytes) |
| projected TAR bytes including headers and padding | 1 GiB (`1,073,741,824` bytes) |
| total member count | 16,384 |
| object count | 16,000 |
| decoded I/O Record count | 1,000,000 |

The total member count includes the two required protocol members and all
object members. Writers and readers MUST apply the same limits. Size arithmetic
overflow is an invalid bundle, not a reason to wrap, truncate, or continue.

## 7. Reader behavior

A Bundle v1 reader MUST reject the complete bundle when it encounters any of
the following:

- a missing or duplicate required protocol member;
- a duplicate object reference;
- an unknown member;
- a non-UTF-8, absolute, non-canonical, or malformed member path;
- a malformed object path or unsupported `ContentRef` algorithm;
- a descriptor or record stream that violates the CBOR v1 contract;
- a record that names an undeclared Connector;
- a missing reachable object or an object digest mismatch; or
- any resource limit violation or arithmetic overflow.

Readers MUST NOT recover a partial bundle by ignoring invalid, duplicate, or
unknown members. Bundle validation is a boundary check over untrusted input.

## 8. Versioning

Bundle version and CBOR wire version are independent compatibility surfaces.
This document defines Bundle v1 and CBOR v1 happens to be its required logical
encoding. A future CBOR version does not implicitly create a new Bundle
version, and a future Bundle version does not implicitly change the Semantic
Core or the State + I/O Protocol v1 compatibility semantics.

Bundle v1 has no separate version member. Readers recognize it by its exact
required member layout and the Descriptor v1 wire value. An incompatible
container revision MUST define a new bundle specification and an unambiguous
recognition rule; it MUST NOT silently reinterpret a Bundle v1 archive.

## 9. Conformance

The reference implementation is `crates/capsule/src/protocol_bundle.rs`.
Descriptor and record byte conformance is tested separately by
`capsule-codec`, including committed golden vectors. The cross-job Protocol E2E
transfers only the Bundle v1 artifact between producer and consumer jobs and
then proves replay, actual egress comparison, and continuation.
