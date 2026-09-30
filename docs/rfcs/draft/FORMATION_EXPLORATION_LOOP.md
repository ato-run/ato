# Formation exploration within frozen ceilings

Status: implementation in progress; acceptance and new 100-app measurement pending.

This draft implements the owner's 2026-09-30 instruction. It supersedes the
one-proposal-round restriction of FORMATION_ADAPTIVE_ESCALATION for explicitly
configured exploration searches, not existing frozen searches.

## Identity and authority

K and the source closure are frozen once. A canonical D includes execution
requirements: exact network endpoints and resource/operation requirements,
scoped to dependency acquisition, build, or runtime. Changing a requirement
changes D's digest. Evidence supporting a requirement is separately addressable.
Exploration ceilings and approval are not part of D and cannot travel with it.

The externally configured exploration ceiling is frozen with SearchPolicy.
Normal-run policy is not edited. Each attempt receives only the intersection
of the ceiling and that D's requirements. No production Binding/credential or
resource is inherited. An unenforceable requirement is refused before start;
host-unrestricted dependency-resolution is not scoped exploration networking.
An out-of-ceiling proposal is saved as exploration_authority_exceeded.

K reached, successful D submitted, risk assessed, approved, published, and
deployed are distinct states. A receipt does not grant normal execution.
Approval refers to the final D digest. Only a Runtime-authenticated fresh
receipt for frozen K and that exact D can update the successful submission.

## Rounds and durable transitions

External formation.max_rounds is a positive integer, default 3. Its effective
value and all other ceilings are stored before the first round. Known D uses
the existing attempt budget. Opening a generation round atomically consumes
one round even if the provider declines, returns invalid bytes, fails, or times
out. Dependency/network/authority repair and successful-D reduction executions
each require a new round. A bounded inspection request settles its generation
round; subsequent inference consumes a new round and a separate provider call.

All rounds are append-only records with existing owner scope, revision CAS,
claim fences, deadlines, source cost checks, and usage reservations. Restart or
linked continuation does not reset counters. Outstanding external effects in
UNKNOWN block further execution until the existing cessation resolution.
Duplicate D without a new verified retry basis terminates as no_progress.

Known D is attempted first. Absence/exhaustion or a repairable failure opens
CandidateProducer regardless of the legacy OperationCatalog's emptiness.
Typed proposals name source refs, runtime/toolchain, dependency/build/process
or source-to-OCI operations, state, ports, and scoped requirements with evidence.
The Rust authority validates and canonicalizes; registered adapters lower the
result. No provider output is executed as shell. Unsupported adapter/toolchain
and out-of-ceiling requirements are explicit diagnostics, not LLM failures.

After PASS, hold the verified D while spending remaining rounds on requirement
reduction. A verified reduced D replaces it; a failed/unknown reduction never
replaces the prior success. Report tested reductions, not mathematical minimality.

## Acceptance and measurement

Use the actual Coordinator, Runtime, and providers for small preregistered apps:
zero-known-D PASS, evidence-based dependency repair, in-ceiling network repair,
out-of-ceiling authority refusal, reduction PASS/FAIL, round limits/restart,
UNKNOWN and retained replay. Persist source/prompt/model/code/config pins,
provider calls/usage/latency/cost, exact D/receipt, all requirement changes, and
no-call reasons. Test doubles do not complete these gates.

Then freeze a new 100-app arm using the unchanged cohort and historical ledgers.
Report additional PASS, actual provider invocation rate, authority recovery,
cost per additional PASS, and any budget/adapter limits. No deployment or remote
migration is included. The pre-existing residual model budget is not reset.
