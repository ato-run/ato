# Local Capsule Repository

Status: Accepted

Each authored project has one `.capsule/` computation repository:

```text
.capsule/
  objects/
  refs/heads/
  records/
  runs/
  protocols/
  contracts/
  bindings/
  provenance/
```

Objects and ComputationRefs are immutable. Branch refs are atomic mutable
pointers. Updating one branch preserves sibling refs and all old objects.
Active Run metadata is separate from sealed branch heads.

Starting a Run transactionally claims a tokenized `starting` lease before any
worker is spawned. Only that token may publish `active`, advance the live Run
head, or release the lease. Concurrent resume attempts therefore cannot create
multiple workers behind one `active.json` pointer.

Selectors are parsed independently from clap:

- `demo` selects `demo@main`;
- `demo@experiment` selects that branch;
- `demo@main#42` selects `head_after` Record 42 on `main`.

Records use stream-local monotonic sequence, explicit causal parents, Adapter,
Protocol, Port, direction, payload ref, `head_before`, and `head_after`.
Wall-clock observation time is informational and has no ordering or
Computation identity authority.
