# Object Closure Bundle

Status: Accepted

`ato-objects` owns verified CAS persistence, resolution, closure traversal,
local refs and Records, transport, signatures, and garbage-collection
boundaries. Semantic identity remains exclusively in `ato-computation`.

A portable `.capsule` uses bundle version 2:

```text
BundleIndex {
  version: 2,
  root: ComputationRef,
  objects[],
  materializations[] { materializer_id, descriptor_ref },
  signatures[]
}
```

The Capsule identity is always `root`. Adding a compatible snapshot to a
replay bundle changes bundle bytes and Materialization inventory, not Capsule
identity.

Objects traverses computation closure through registered
`ComputationReferences` and Materialization descriptor closure through
registered `MaterializationReferences`. It does not decode concrete Compose,
Replay, Snapshot, Workspace, or HTTP schemas itself.

Import verifies canonical JCS, version, limits, duplicate and path-like refs,
descriptor sizes, hashes, signatures, complete closure, and absence of
injected unreachable objects in temporary storage before inserting anything
into the destination CAS.

Export includes the selected computation closure and every explicitly
requested Materialization closure. A failure in any requested Materializer
fails the complete `encap`; sibling-branch-only history is not exported.
