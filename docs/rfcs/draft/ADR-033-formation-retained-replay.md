# ADR-033 — Retained candidate replay

Status: implemented on the unmerged foundation stack; not deployed.

A retained candidate is not a Capsule identity and not an old PASS reused as a
new verdict. `ato.retained-candidate/1` is canonical, content-addressed provenance
with explicit `static_web` or `process_workspace` shape. It contains canonical
K/D, effective Browser K relation, original source closure, artifact transport
ref/size/expanded size, original materialization ref, creation attempt and a
validation profile. Runtime bindings contain resolved toolchains, not copied
argv/cwd/env semantics. Existing materialization refs retain their meaning:
static manifest digest or process workspace tar digest. `retained_ref` is separate.

Publication runs after common K verification: prepare archive, measure/validate,
reserve owner-scoped object, stream upload, rehash stored bytes, finalize immutable
ready descriptor, report. Failure preserves verification evidence but produces no
VerifiedRoute. New Rust Runtime executions publish before reporting success;
legacy v0 source-only reports remain readable/acceptable for compatibility.

API migration 0299 creates owner-scoped retained storage in a separate R2 namespace
from source objects. Digest knowledge is not authorization: publication needs the
current claimed runtime/attempt/fence and fresh accepted receipt. Download needs
a fresh claimed retained ticket, exact selected K/D, owner and ready object.
Descriptor lookup is owner-authenticated; there is no public digest download URL.
Readiness atomically rechecks the assignment. Ready descriptors cannot be changed.

Typed ticket input explicitly distinguishes source Formation from retained replay.
Legacy v0 source tickets are adapted on read, without rewriting persisted bytes.
Retained requests have no source/capsule TOML requirement. Missing objects fail
with a typed error, never implicit source fallback. The Rust requester example
`formation-worker/examples/retained_replay.rs` drives this path; no UI is added.

RetainedCandidateRealizer validates provenance, transport digest, logical/expanded
sizes and safe archive paths before launch. Static manifest/blob validation is
also repeated. It allocates fresh runtime scratch and shares CandidateLauncher,
run_attempt, browser verification and verify_observed_candidate with Formation.
It neither detects nor authors nor builds. Process toolchains must already be
available. A prior receipt is never supplied as the new verification input.

Persistence authority is D1 plus R2, not Runtime out_dir. Coordinator restart and
Runtime cache loss do not discard objects. Local caches remain non-authoritative.
This does not migrate Hosted lease/fence, change ownership or deploy anything.
