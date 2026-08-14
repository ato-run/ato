# Object Closure Bundle

Status: Accepted

`ato-objects` owns verified object persistence, resolution, traversal,
transport, signatures, and the GC boundary. It never defines semantic
identity; that belongs to `ato-computation`.

A `.capsule` is a portable object bundle:

```text
BundleIndex {
  version,
  root: ComputationRef,
  object descriptors,
  optional signatures
}
```

The Capsule identity is always `root`. Envelope bytes are not identity.

Traversal is dependency-inverted. `ato-objects` implements the graph
algorithm; each semantics registers an outgoing-reference extractor for its
own residual encoding. Compose discovers child `ComputationRef` values;
workspace discovers its source closure and files.

Import verifies canonical encoding, limits, duplicate/path-like references,
descriptor sizes, object hashes, signatures, closure completeness, and
reachability before inserting into the destination CAS. Export starts at the
root and includes exactly the reachable closure.
