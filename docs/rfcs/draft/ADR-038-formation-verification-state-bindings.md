# ADR-038 — Explicit state bindings for verification

Status: draft; implementation and real acceptance in progress; not deployed.

Source exploration currently creates an empty, attempt-owned state working copy.
It deletes that copy with the candidate. This remains the default. It cannot prove
application persistence or satisfy an application that needs owner initialization.
Successful D, a filesystem path, and a writer fence are never Run authorization.

An explicitly authorized functional verification may borrow existing Runtime
state attachments. The trusted caller must first establish owner, assigned Run,
lease, current writer generation and the intended isolated verification instance
through existing control-plane state services. Production state is not inferred
or looked up from a Search or D. No new state store, identity or package preset is
introduced. A host path cannot be supplied by a proposal or a public descriptor.

The Runtime-only binding joins the existing `LaunchContextV1`, logical
`StateAttachmentV1` and private `ResolvedStateAttachment`. An explicit slot ID
maps a Derivation's requirement to its assigned state key; those identifiers need
not be equal. Logical/private key, revision, access and mount must agree. Every
declared slot must be bound exactly once, and unexpected or duplicate bindings
are refused. Read-write bindings require a positive assigned writer fence;
read-only bindings cannot carry one. A fence is non-secret evidence, not authority.

Working copies must be existing canonical directories, disjoint from source and
candidate cleanup trees. The caller holds their exclusive ownership/lease and
prevents replacement while borrowed. The normal process executor mounts the
working copy at the declared guest path, with unchanged isolation and network
policy. It uses the assigned launch context, not fabricated instance/Run IDs.
Candidate stop destroys only candidate scratch. State commit/release/quarantine
remain the existing control-plane caller's responsibility; uncertain stop must
not release or reassign a writer. Paths and values are not serializable and never
enter K, D, Capsule identity, public evidence or provider input.

The common retained realizer can carry this explicitly authorized verification
binding into the same launcher and fresh frozen-K verification. Source exploration
and the production Worker continue using no attached state until authenticated
assignment transport is connected; unbound persistence is not advertised as a
capability. Existing source-owned migration may operate on an explicitly bound
working copy in the runtime phase, without app-specific commands or source edits.
Owner bootstrap is a separately recorded functional action, never a weaker K.

Acceptance requires: mismatch/duplicate/missing/fence/path refusals before launch;
actual contained process writes and reads after stop/restart; confirmed cleanup
without state deletion; current-owner/current-assignment and stale-writer refusal
in real Coordinator storage. Kutt additionally requires source-owned migration,
private temporary JWT, owner initialization, same original K and representative
UI/state preservation. Unit tests alone do not complete those gates. Neither a
successful verification nor this API enables ordinary Run, publication or deploy.
