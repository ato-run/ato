# Capsule bundle

For the concept, start with [Capsule and Run](concepts/capsule.md).

The current implementation identifies a Capsule by one immutable
`ComputationRef`. A portable `.capsule` file is transport, not identity. Bundle
version 2 contains the root ComputationRef, its verified object closure,
optional Materialization descriptors, and optional signatures.

```text
Capsule identity = root ComputationRef
Capsule identity != bundle bytes
Capsule identity != Materialization inventory
```

Different bundle encodings or compatible Materializations of the same root
refer to the same logical Capsule. Import verifies the complete closure before
inserting objects.

```sh
ato encap demo@main --materialize ato.replay@1 -o demo.capsule
ato run demo.capsule
```

See [Object Closure Bundle](rfcs/accepted/OBJECT_BUNDLE.md) and
[Capsule Bundle](rfcs/accepted/CAPSULE_BUNDLE.md).
