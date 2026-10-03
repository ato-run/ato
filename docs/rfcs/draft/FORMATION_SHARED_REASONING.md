# Shared reasoning prototype

This unmerged draft implements the user-requested Codex-first prototype.
Historical ledgers and provider configurations remain immutable. The selected
OSS and cases are preregistered in formation-codex-prototype-selection.json;
executable, input, output and resource hashes are frozen before actual execution.

The common CandidateProducer input contains frozen K/source identity, bounded
public inventory and acquired source context, operation/toolchain capabilities,
the frozen sandbox ceiling, past canonical D/validation/execution evidence and
remaining rounds/attempts/inspection/provider budgets. Only verified,
credential-excluded source-relative inventory entries are public; physical
source paths, private authorization maps, credential values and host identity
are not. Every model-visible input and exact output is persisted with hashes.
Omitted/truncated acquired context is explicitly recorded.

An opt-in requester-side reasoning driver accepts the existing typed outputs:
inspection request, D proposal or bounded reasoned decline. Inspection requests
are validated by the common Rust validator and retrieve only frozen authorized
bytes. Each round can make bounded inspection exchanges before one D. Inspection
count, source bytes, inference input/output, calls and elapsed time are separate
budgets. Initial manifest/entrypoint and earlier inspection results stay acquired;
the projection remains bounded. An explicit frozen round timeout accommodates
inspection/session transport without changing default max_rounds=3. Legacy
round timeouts and wire bytes are unchanged.

The session adapter exchanges JSON files with Codex; it cannot execute commands.
Its delivery uses the existing untrusted fixed-byte receiver transport, while a
separate durable inference journal records codex_session and exact input/output.
This is not a fixed producer pretending to be an API call: session token usage
and price are unavailable, never fabricated. Only ordinary validator → canonical
D → lowering → admission → Runtime → frozen K Verifier executes the proposal.

After a Codex actual OSS gate, the DeepSeek adapter consumes the same bounded
input and inspection protocol from the same initial information. Codex success
D/history is not a seed. Actual sends use the existing dollar reservation journal
with unique per-exchange call keys; no credential enters common context. The
receiver's per-round provenance aggregates exchange usage, while the requester
journal remains the authority for actual call count and each request/response.

Worker-composed proposal/2 carries validated inspection refs alongside the final
unchanged proposal. It is only admitted under opt-in frozen reasoning limits;
the receiver independently validates all refs and compiles the final D with its
same Rust authority. Inspection does not manufacture a D or receipt. Durable
request/response journals preserve acquired context and consumed budgets on
restart. Unresolved API sends are conservatively charged and never resent.
Claim/fence state stays owner-private; pending/UNKNOWN cannot be bypassed.

Success remains k_reached_awaiting_assessment. Reductions consume later rounds
and replace the success only with a fresh same-K PASS. No approval, normal Run
grant, publication or deployment follows. App-specific presets, source rewrites,
hidden setup and out-of-ceiling authority are prohibited in this prototype.

Prototype configuration uses `ato.formation-exploration-config/2` and optional
`exploration.reasoning` with frozen `round_timeout_ms`, `inspection_timeout_ms`
and `inspection_source_bytes`. Legacy config/1 cannot extend its timeout.
An inspection exchange counts toward the owner-local call and inspection budgets,
not toward another proposal round. Total session exchange latency includes the
file bridge wait; API latency is measured around each actual transport call.
`reasoning-pilot.py` reuses the existing real Coordinator/Runtime controller.
`reasoning-session.py` atomically delivers one input-digest-bound typed response;
it never starts an app. Exact inputs and response records are hash checked on
restart, including source/K identity and verified source text prefixes. API
protocol/transport failures halt sends and preserve the journal's settlement;
the recorded failure is replayed as evidence, never resent to the model.

An optional bounded owner-authored `reasoning.goal` freezes an acceptance protocol
(e.g. a least-privilege egress probe) and appears verbatim in the common input.
It grants no execution rights and cannot change K or the ceiling. Both providers
receive it; deliberate probes are reported separately from accidental failures.

## Pre-dispatch call allowance exhaustion

The per-Search exchange allowance is independent of the maximum reasoning rounds.
Inspection and validation exchanges consume their actual call allowance without
consuming an extra round. Reading a saved input/answer can recover delivery; a new
input must be refused after the original allowance is consumed, including after
Requester restart. API transport cells count against the same provider ceiling.

A refusal before dispatch is an owner-local operational fact, not a provider
call or an inference result. The claimed completion uses legacy `provider_error`
with `pre_dispatch_error: call_budget_exhausted`, no raw output or provenance.
The receiver persists the fact with the original completion fence and projects
`reasoning_call_budget_exhausted` into the authenticated round diagnostics. Rust
returns terminal `budget_exhausted`; expired clocks still yield
`deadline_exceeded`, previously satisfied K still yields an assessment-pending
submission, and unresolved workload effects keep their existing UNKNOWN fence.
No allowance, round, Search deadline, reservation or cost is reset. Old records
with no pre-dispatch fact retain their original classification. Receiver support
and its additive migration must precede sending the new optional field.
