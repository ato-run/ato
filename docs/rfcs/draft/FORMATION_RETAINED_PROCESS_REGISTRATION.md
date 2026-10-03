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

## Separately authorized functional verification (implementation extension)

An owner may approve a fresh functional verification Search for the exact retained
artifact/K/D. This permission is distinct from assessment and ordinary Run
permission. The Coordinator creates an Instance using the same registry and state
namespace, grants the existing StateService writer, and persists the common
launch spec/digest on an immutable functional Run mapping before issuing a ticket.
Historical tickets omit `verification_state`. Private variables are assigned to
this fresh Search; no original exploration assignment is copied.

The Runtime reuses retained replay and `LeaseStateArtifactTransport`. It restores
one declared filesystem slot, verifies the original K, executes only separately
approved bounded HTTP Adapter operations, confirms workload stop, and commits
state through the same writer fence. The durable attempt finish is deferred until
this entire interval settles. Disconnect after K therefore leaves UNKNOWN and
cannot replay an accepted HTTP action. This path does not create a normal Run
VerifiedRoute. The same registration/Instance is reused for a separately approved
next Run; it receives the StateService head revision and a new fence.

The initial functional action contract permits at most four GET/POST operations
with fixed relative paths, declared private JSON bindings and status assertions.
The explicit ceiling must authorize each HTTP operation. Additional private
binding metadata cannot replace D's bindings or permit artifact embedding; K and
D are not rewritten. Cookie/session authentication, response-id binding, method
extensions and body assertions are not represented by this initial contract.
Consequently it is not yet sufficient evidence for Kutt link create/edit or
changedetection watch create/edit/stop. Those product gates remain open and need
an approved concrete action plan plus the corresponding common HTTP Adapter
contract. Synthetic state/receipt fixtures are not real application acceptance.

No remote migration, deployment, flag enablement or ordinary Run permission is
part of this implementation. Actual functional execution requires the separately
presented Source/artifact/K/D, Runtime, fresh input scope, operation/attempt limits
and deadline to be approved before starting.
