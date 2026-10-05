# Formation retained process registration

Status: implemented; product-path acceptance measured. The Source Result extension
and owner-only `ato form-verify` path are merged and deployed. Saved Kutt acceptance
and separately authorized staging/production controlled-Source Run A/B receipts
are described in the [release gate record](../../ops/formation-v0-release-progress-20261006.md).
These records do not grant ordinary Run, publication or fork permission, and the
100-OSS/independent API release gates remain in progress.

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

The legacy API owner-session entry accepts an explicit functional-verification request
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

The initial local bridge alone did not prove PWA resume or changedetection/Kutt functionality.
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

The functional action contract permits at most four typed HTTP operations under
an explicit ceiling. Additional private binding metadata cannot replace D's
bindings or permit artifact embedding; K and D remain unchanged. The common
HTTP Adapter extension defines cookie/session authentication, scalar response
capture, bounded HTML leaf/key selection and boolean body/header comparisons
in [FORMATION_FUNCTIONAL_HTTP_BINDINGS.md](FORMATION_FUNCTIONAL_HTTP_BINDINGS.md).
The exploration GET/POST catalog remains unchanged and rejects these advanced
fields outside a separately approved functional plan.

The shared Rust authority compares safe observations with the saved functional
plan: exact request template, logical Port/index, guard, accepted status, and
matched response check metadata in Adapter order. Missing, extra or mismatched
observations cannot support PASS. Captured values are not evidence fields. These
synthetic checks do not prove Kutt/changedetection functionality; their actual
same-artifact create/edit/stop/restart gates require approved execution.

Remote migration and deployment were later separately authorized and measured;
they are not implied by this projection. Ordinary Run permission is still outside
this implementation. Actual functional execution requires explicit approval of
the Source/artifact/K/D, Runtime, fresh input scope, operation/attempt limits and
deadline before starting.

## Normal Source Search anchor

A normal CLI Source Search does not require a pre-existing Source Instance.
Rust validates the saved accepted PASS attempt, immutable Source closure, frozen
K, accepted D, ready retained object and exact target/capability profile, then
projects a canonical `ato.formation-source-result/1` evidence record. The API
persists that projection with authenticated owner and Search/attempt provenance.
This is neither a Capsule identity nor a state namespace. An interrupted save
is repaired from the same PASS without creating a Search, attempt, inference or
deadline. Conflicting records and unavailable provenance fail closed.

The owner-only Source Result view selects the authority's accepted submission,
not the latest retained artifact. `ato form-verify --source-search-id ...
--owner-token-file ... --plan ... --authorize-functional-verification` fetches
that anchor and invokes the existing retained-verification service. No dummy
Source Instance or private dispatch namespace is created. Optional supplied
Source/K/D/retained refs must agree with the saved anchor.

An explicitly approved registration creates a functional-only schema/Instance
with `source_result_ref` and NULL sealed Capsule revision. Registration replay
returns the same Instance/Run/lease/deadline. State belongs to that functional
Instance and slot: another Run of the same registration restores the same
namespace with a new writer fence; another registration gets a separate
namespace. Ordinary execution, publication/install/shared/fork paths reject this
unassessed basis. Success never automatically becomes normal Run permission.

The saved Kutt Search reached this product path without another Native Search.
The separately measured staging and one production controlled-Source flow also
performed Run A write/commit and Run B restore/delete with fence 1→2, followed by
ACK/closed, writer/reservation/input zero. Their exact pins, functional observations
and remaining browser limitations belong to the linked release records, not to
the semantic identity or earlier private fixture results.

## Logical slot and StateService namespace

Source declares logical IDs such as `app.data`; StateService requires a lowercase
physical key. The Rust projection retains compatible lowercase underscore IDs
and maps other valid isolated IDs to `slot_` plus 59 hexadecimal SHA-256 digits.
The prefix is reserved: an authored ID already beginning `slot_` is hashed too,
preventing an authored physical-looking ID from aliasing a mapped logical ID.
This key binds the one declared mount to its existing writer namespace. Source,
K, D and retained descriptor bytes stay unchanged. Ticket admission recomputes
this mapping from the exact D instead of accepting a caller-supplied mount/key.

## Read-only retained preflight

The owner-only retained-content endpoint checks the owned creation assignment,
its Source closure, the exact Rust-validated descriptor and transport digest.
It hashes a first bounded R2 stream and opens the same ETag for transfer; changes
or missing content fail closed. This creates no Run, grant, attempt or budget
charge. Raw content remains owner preflight data and is never projected into
Producer, Session public progress or reasoning input. Availability on a local
final pin does not imply the endpoint is deployed on historical API pins.
