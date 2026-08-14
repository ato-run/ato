# Capsule

A Capsule is a sealed, addressable computation: a `ComputationRef`.

There is no separate Capsule semantic struct. A `.capsule` file is transport:
it contains a root `ComputationRef`, the reachable object closure, and optional
signatures. Import verifies the complete closure before inserting objects.
Different bundle encodings of the same root identify the same Capsule.

```bash
ato encap . --output app.capsule
ato decap start app.capsule
```

See [Object Closure Bundle](rfcs/accepted/OBJECT_BUNDLE.md).
