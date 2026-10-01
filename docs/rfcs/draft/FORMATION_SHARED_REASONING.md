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
