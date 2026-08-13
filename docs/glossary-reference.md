# Glossary

- **ComputationObject**: the only canonical semantic value.
- **ComputationRef**: immutable address of canonical computation bytes; the Capsule handle.
- **Run**: mutable cursor whose head is a ComputationRef.
- **Semantics**: concrete owner of logical transitions and observations.
- **Adapter**: compiler from external authoring/source systems to computations.
- **Provider**: owner of physical realization and materialization.
- **Objects**: verified CAS, resolution, closure traversal, transport, signatures, and GC.
- **Port**: typed interaction crossing the selected computation boundary.
- **Tau**: interaction internalized by composition.
- **Trace/Receipt**: optional evidence about past transitions or realization.
- **Snapshot**: provider materialization; never computation identity.
- **`.capsule`**: portable object-closure bundle rooted at a ComputationRef.
