# Autonomous Formation reasoning (Codex first)

Status: draft implementation contract, 2026-10-01. Supersedes the shared
reasoning prototype's repeated declines and unrestricted inference inventory.
Existing measurements, frozen Search state, UNKNOWN and spend journals remain
immutable. Independent acceptance runs get new measurement identities.

The existing requester, Coordinator CAS/fence, Rust proposal compiler, scoped
sandbox, Runtime and frozen-K Verifier own the entire path. The session and
API adapters exchange the same saved public input and typed output. No manual
source startup, app presets, K edits or post-hoc successful D registration.

## Start

Freeze effective configuration: 3 D rounds, initial operation plus 3 retries,
600000 ms per round, 1800000 ms Search lifetime by default. Smaller explicit
limits are permitted. Search lifetime includes inference, inspection, execution,
backoff and recovery waits; no restart, input wait or continuation extends it.
Inspect prior evidence, outstanding attempts and Runtime capabilities before
opening a round. Reserve round/revision/deadline in Coordinator before work.
Unavailable Runtime waits without a round. Pending/UNKNOWN blocks new attempts.
Recover lost start/claim responses by stable operation identity and status.

## Execute

Root manifests, locks, Dockerfiles/configuration and README are the discovery
origin. A nested path requires an explicit reference from acquired root/context;
record source ref/digest and the reference that widened scope. Directory
existence and COPY . . are not inference authority. Bound glob expansion and
return ambiguity/unsupported topology explicitly. Keep acquired context and
record unseen versus truncated projections; prioritize manifests, recent
inspection and failure-related context without duplicated inventory.

Each round represents one D hypothesis. Inspection, JSON/schema repair,
validation repair and transport retry stay within it and consume separate
exchange/call/inspection/time/cost limits. Persist input/output before submission.
The Rust compiler validates canonical D and phase/resource/operation requirements.
Execution occurs only inside the frozen ceiling. A real attempt produces a fresh
same-K receipt. A different D after execution FAIL requires new evidence and a
new round. Unsupported capability and repeated inspections/declines stop promptly.

Network requirements separate dependencies/build/runtime. Authority declares
phase, logical resource and operation. Requirements participate in D identity;
the exploration ceiling grants no normal Run rights. Over-ceiling proposals
retain their basis and terminate exploration_authority_exceeded. Following PASS,
remaining rounds may reduce requirements only; update the successful D only
with a fresh PASS and keep the former success on reduction FAIL.

Variable requirements contain metadata, never values. Resolve registered values
only within application/resource/operation/phase/service/account scope and expiry.
Suitable temporary values are private runtime bindings, not embedded in D.
Distinguish build-only placeholders from values satisfying runtime K. External
authentication necessary for K cannot be substituted with a dummy. Missing or
ambiguous/expired/out-of-scope bindings stop needs_input with purpose and obtaining
instructions. Owner input selects this Formation only or reusable scope. Store
encrypted values separately from metadata, revoke/delete explicitly, and delete
Formation-only values after attempts and cleanup are conclusively finished.

Source-owned Python requirements may be resolved in the dependency sandbox into
downloaded artifacts, recording versions and SHA-256. Install only those fixed
artifacts offline; no requirement to pre-hash the source repository. Native
builds or unavailable operations produce concrete capability evidence.

## Recover and finish

Journal retries by stable operation ID before dispatch. Lost completion is
queried or resent with identical bytes; never rerun inference/Runtime. Lost
attempt response is queried by attempt ID. Uncertain started execution stays
UNKNOWN. Lost provider response preserves sent state/reservation: use provider
idempotency/result lookup when supported; otherwise each actual resend is a
separate charged call and the uncertain call stays conservatively charged.
429/temporary 5xx honor Retry-After within retry/deadline/cost bounds. Provider
auth/config errors are infrastructure failures. Fence/revision conflict reloads
current state. An unresolved execution cannot be bypassed with another D.

Commit evidence/budgets/success/next state together under Coordinator fencing.
PASS is k_reached_awaiting_assessment, never publication, deployment or Run
approval. Keep repairable FAIL distinct from no_progress, source_broken (requires
reproduction evidence), needs_input, unsupported capability and infrastructure.
Preserve phase timings and all historical evidence. Record actual acceptance,
fixture tests and unverified behavior separately, with executable/API pins and CI.
