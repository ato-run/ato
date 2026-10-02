# Bound HTTP operations on an owned logical Port

Status: draft. Builds on accepted PROTOCOL_ADAPTER and existing encrypted
variable metadata, assignment, Runtime grant, common launch and HTTP Adapter.
No application preset, new Kernel semantic primitive, source rewrite or weaker K.

Some source-declared applications require input through their running HTTP Port
before the original K can be observed. This is an Evolution owned by the HTTP
Adapter; GET is not assumed to be a semantic no-op. A typed source proposal may
declare bounded operations from rooted source references, including an explicit
status guard. The declaration binds a logical Port, relative path, method and
JSON field-to-input-slot references. It never carries credential values.

The compiler preserves authored order in D; empty operations preserve old D
bytes. K remains frozen and independent. The normal common realizer invokes the
operations after contained launch/readiness and before first K observation.
Source and retained paths must share that invocation. Unsupported/unbound
launch routes refuse the declaration; an ignored operation is not successful
execution. Normal Run, risk assessment and publication are separate decisions.

Only the already assigned logical Port's loopback endpoint is a transport
target. Proposals cannot name hosts, absolute URLs, redirects, query credentials
or private filesystem paths. Runtime `ato.http@1` execute authority must be
declared in addition to bind authority. A capability is advertised only where
the operation implementation and required input-grant path are bound.

JSON input references use existing declared Runtime variable requirements and
redeemed private values, with original scope, expiry and assignment checks.
Missing/ambiguous/revoked/out-of-scope values stay needs_input; no fallback value
or credential lookup is inferred. Protected inputs are non-embeddable. Input
values and response bodies/headers, including returned tokens, are never put in
LLM input, D, helper artifacts, public evidence or HTTP capture payloads.

The existing HTTP Adapter wire encoder sends a private, non-serializable request.
Separate safe request-template and numeric response-status observations retain
the Evolution/evidence distinction. Neither secret hashes nor raw payloads are
identity. Every I/O checks the original remaining budget; caller-owned deadline
and retry journals are not reset. Requests are not automatically replayed based
on HTTP method vocabulary. A lost mutating response cannot justify another POST.
The common attempt journal already prevents restart from executing the same
attempt again. Reporting and confirmed cleanup remain possible after expiry.

Declared guards permit a source operation to be skipped when its explicit
observed condition does not hold. They do not weaken K or infer success. HTTP
failure feeds bounded status/transport evidence to subsequent reasoning; no
response body or input value is exposed. Known cleanup and uncertain termination
retain their existing distinct treatment, including UNKNOWN search hold.

V0 is one service with at most four HTTP JSON operations and thirty-two input
fields each, bounded payload/response line and existing round/Search limits.
No Compose orchestration, source/Dockerfile rewriting, arbitrary command or
external-service credential validation is introduced. First acceptance uses an
owned local account and actual C2/Runtime; an external account requires explicit
user authorization. Native dependencies, migration, owner interaction,
same-K receipt and persistence are recorded separately. Independent API arm
starts with identical initial information, without a Codex D seed.

Implementation stages: private HTTP Adapter transport and adversarial component
tests; canonical proposal/authoring and common realizer integration; strict API
compiler/WASM projection and capability disclosure; actual owned-account and
state acceptance. A stage is not advertised as a completed later stage.

The Formation authoring DTO describes intent and bounded safe evidence; the HTTP
Adapter retains its own wire DTO and private encoder. The common Runtime maps
between them, preserving dependency direction. A request-template observation
records intent to dispatch; only a distinct response-status observation proves
a status was received. Neither observation claims K satisfaction.

Proposal items are `runtime_port_operations: [{operation, basis}]`, with a
previously inspected SourceReference for each item. Every referenced variable
must declare Runtime phase, the same logical Port resource and execute operation,
and `artifact_embedding:false`. Status lists are nonempty, unique and ascending.
The current capability fact is `formation.http_port.operations.v1`; it is true
only for the contained scoped Runtime with its grant path attached. Catalog
binding defaults false and is omitted from old canonical authorization. Prompt
v13 and capability schema 5 expose the field; original prompt bytes and older
capability projections remain preserved. Static, OCI, legacy projection and
portable profile paths refuse unbound declarations instead of ignoring them.

Local validation covers Adapter transport (6), Formation library (122), proposal
registry (34), authoring identity/roundtrip/privacy (3), common Runtime (95),
Worker (78), portable bundle (94) and authority ABI (7) tests. Architecture
check passes 43 packages; CLI/Worker/Runtime/HTTP/portable all-target clippy passes.
Tests use actual owned loopback sockets for private JSON, explicit guard
match/skip, redirect refusal, lost mutating response, expired allowance and no
replay. The common attempt separately preserves confirmed-stop/scratch-kept,
finished journal and safe failure evidence without a K receipt. These are
component/compiler checks; final owned-account Coordinator/Runtime and
independent API app acceptance remain outstanding. No external service account,
ordinary Run, publication, deployment or remote migration is authorized here.
