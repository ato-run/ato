# Formation retained process registration

Status: Draft; local implementation only. No deployment, migration application,
feature enablement, production assessment or live acceptance is authorized.

A fresh same-K exploration submission is evidence, not a normal Run grant.
The new `project_retained_registration` operation checks the persisted
SearchState, its exact fresh submission, and canonical retained descriptor
through the common Rust authority. Source closure, K, D, artifact digest and
creation attempt must agree. Unknown fields, noncanonical descriptor bytes and
unsupported scopes are refused without input-bearing diagnostics.

The projection supplies the already-built process argv/cwd/env, resolved runtime
bindings, HTTP logical readiness and one declared filesystem slot. Physical path
and environment construction is shared with existing Runtime lowering; no new
source detector, planner, state engine or Contract evaluator is introduced.
An HTTP observation path is a logical endpoint, never an artifact existence test.
The old descriptor schema and digests are unchanged.

Only a process workspace with exactly one writable filesystem slot and one
HTTP export is admitted. OCI, static routes, multiple slots, private variable
bindings, Runtime Port operations and Runtime network access fail closed because
the existing ordinary process launcher cannot preserve their scoped authority.
These exclusions are capability limits, not proof that those applications fail.

The API owner-session entry accepts an explicit functional-verification request
and an existing owned Source Instance. It must prove that the source namespace
already refers to the same immutable closure, and that the caller's selected
Linux target matches the creation attempt's immutable capability profile. It
registers a new process schema and verification Instance using the existing
registry and Instance service. Registration keeps the existing sealed namespace
point; it never invents a ComputationRef or treats an artifact/receipt as identity.
The exact exploration K/D remain in the immutable registration evidence.

An independent owner placement confirmation and ordinary Run authorization are
still required. State is allocated at dispatch by the existing StateService:
writer acquisition, fencing, lease-scoped restore, quiescence before commit and
new-Run restore retain their existing authority. The lease resolves retained
bytes for its exact registered schema and workspace digest; the R2 key is never
projected to the Producer. No app-specific mount or manual state grant is needed.

This local bridge does not prove PWA resume or changedetection/Kutt functionality.
In particular, a Kutt descriptor containing scoped private initialization cannot
be silently downgraded into this process route. Live functionality, same artifact
restart, private authentication, and final-pin PWA input/resume require separate
approved acceptance plans. Historical fixture results and measurement pins are
unchanged.
