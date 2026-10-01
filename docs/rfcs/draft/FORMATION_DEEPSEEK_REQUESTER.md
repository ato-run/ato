# D2-C — requester-owned DeepSeek CandidateProducer (mock verified, not live)

## Scope and pins

- C2 merge: `d4fa39a693eb253949c65486288908e9ae47cf57`.
- D1 API #701 merge: `9fa4be45adf7dbda1fa68689c365a1806921569f`.
- D2-A ato #1426 merge / this branch base:
  `c4bb53564669065aa6c47d1ed283e02ac6fa68ae`.
- D2-B API #703 merge / **fixed acceptance receiver pin**:
  `38668a97e7632256b33074b0d223670c16b76bfd`.
- Receiver WASM source is the D2-A merge above; SHA256
  `d4732b46f1ce5ab3d1193d957d7bcb2d24f0469ffaa80e267c6cfda5bccb8a46`.
- Implementation / actual-tested source:
  `be6acd8a547ae4077035e2a551b3c0a78cc84bc4`.

D2-A was merged only after exact-base CI comparison and explicit user approval
of its exact-head admin merge. D2-B integrated concurrent API main #702
(portable attempt lifecycle / independent 0306), then repeated typecheck and
all 312 receiver tests successfully. GitHub API CI was not started due to the
billing/spending limit annotation; it is **not green**.

D2-C does not change C1/D1 storage, claim fences, completion CAS, compiler,
CandidateRegistry, K or Verifier. It does not add Escalate, a repair loop,
Jev generation, new operations, or remote rollout. Original Formation Phase 4
remains the target: zero known D / no Preset; the live general-LLM gate has
**not** run. Mock provider integration is not LLM efficacy or coverage proof.

## Versioned input and source authority

`ato.formation-proposal-request/1` and fixed wire remain source-free, unchanged.
D2-A `ato.formation-proposal-request/2` contains all /1 fields and
`source_context: [{source_id, kind, logical_id, encoding, truncated,
content_sha256, text}]`. `kind` is entrypoint/module; encoding is only `utf8`.

Explicit `enable_candidate_producer` rechecks the C2 archive digest and source
closure and extracts its regular-file inventory. Context is captured **there**,
only for existing owner-authorized source-domain entries. For a module, a
verified `module.py` is preferred; otherwise the verified package's
`__main__.py` is used (package-parent checks remain mandatory). No host module,
working-tree reread, symlink, arbitrary file, README/manifest crawl or new
allowlist. The bounded excerpts are kept privately with the submission.

Policy is false/0 or true/1..65536. Total transmitted text <=64 KiB, individual
text <=8 KiB. Equal deterministic shares prevent one source starving others;
truncate only on UTF-8 boundaries. Invalid UTF-8/binary is omitted, never
lossily decoded or base64-encoded. Source IDs hash content/kind/opaque ID, not
private paths. Excerpt digests identify exactly the transmitted text.

The request uses closed failure/inspection summaries, catalog and remaining
budgets. No private source mapping, credentials, bindings, receipts, logs or
host identity are added. Owner-authorized source itself is untrusted data;
prompt instructions are not a security boundary. Ato validation rejects an
unauthorized proposal regardless of the model's reasoning.

## Provider interface and transport

`CandidateProducer<Request = ProposalRequest, Output = ProducerOutput,
Error = ProducerError>` preserves all old implementations and call sites.
Only generic Rust type parameters were added; no provider SDK, product/model,
HTTP, key, or identity semantics entered the core. DeepSeek implements the
same trait for request/2 and a requester-owned output/failure envelope carrying
D1 operational provenance. Compiler output remains untrusted raw bytes.

`DeepSeekConfig` requires provider, model, endpoint, prompt_version,
max_output_tokens, timeout_ms and ThinkingMode. No semantic model/default.
Production endpoint is exactly `https://api.deepseek.com`; redirects, retries
and environment proxies are disabled. Credentials are an explicit environment
variable name, read only **after** a durable spend reservation. No key in argv,
request JSON, errors or logs. `new_mock` accepts only a literal loopback HTTP
root and uses a built-in synthetic key; it never reads an environment key.

One POST `/chat/completions`: non-streaming, JSON-object response mode, explicit
thinking disabled or enabled with low/high/max effort, output <=2048 tokens,
timeout <=30s. The user message is canonical request/2 JSON; source is not
re-embedded in prose. The fixed versioned system prompt is:

- version: `ato.formation-candidate-producer-prompt/1`
- SHA256: `a3217b28b4242fdc03e11fe5dee7d91fbbc47b79358c4ae31601002b7a81ea50`

The adapter extracts exact `choices[0].message.content` UTF-8 bytes only.
No markdown stripping, substring extraction, field repair, second call or
fallback. Content >16 KiB is rejected; the entire provider envelope is capped
at 128 KiB before parsing. Reasoning/ancillary vendor data is never persisted.
Malformed proposal JSON is a **transport success** which Ato rejects as
`invalid_output`, not a fabricated HTTP error or K verdict.

Closed errors: transport_error, provider_refused, timeout, malformed_response,
response_too_large. Non-2xx bodies are not read. Success needs integer input
and output usage; missing/invalid usage is malformed_response. Error usage is
null unless actually observed. Cost is an explicit cache-miss upper estimate
from the configured snapshot, not a provider billing receipt or identity.

Protocol reference: [DeepSeek Chat Completions](https://api-docs.deepseek.com/api/create-chat-completion/).
D3 v2 now pins the live model, prompt and peak price caps; see the
[preregistered plan](../../ops/formation-deepseek-d3-plan-v2.md) and
[actual live results](../../ops/formation-deepseek-d3-results.md).

## Durable requester path and restart

General and fixed paths share **one** existing claim engine. No call before
successful private-fence claim; a failed/lost claim consumes the local latch.
The general transport uses the remaining timeout minus a 500ms completion
reserve. If insufficient time remains, no provider call is started.

Submit exact raw content and versioned provenance once; uncertain completion
is recovered by GET only. No completion/provider retry. The independent Rust
recompiler compares ordered outcomes, proposal IDs, D/recipe/candidate and
frozen K before registering a recipe through the ordinary execution path.
Only an actual Runtime observation plus Verifier receipt permits acceptance.

Before a general round, `configure_general_producer` pins provider/model/prompt.
Saved `provider_call` is strict-parsed against that config and raw provenance.
Within a process, the complete observed usage/cost/error envelope must also
match persisted evidence. On restart, independently unknown usage is not
invented: config, D1 consistency, and Rust raw recompilation are checked.
Fixed/general downgrade or unexpected general provenance fails closed.

## USD 5 guard (D2 synthetic tests; D3 live evidence recorded separately)

`CallBudget` is an acceptance-side, append-only reservation journal, not a new
Formation semantic budget. Explicit peak/cache-miss prices and input/output
caps reserve worst-case cost with upward rounding. The whole max-call plan
must fit its explicit ceiling, itself <=5,000,000 USD micros. Request bytes
plus the prompt and a conservative framing allowance must fit the input cap.

File locking plus append-and-fsync occurs before key read/send. Never refund
unknown usage or timeouts. Reopening verifies the exact plan; duplicate cells,
exhausted calls, changed prices/plans and partial/corrupt writes fail closed.
Creating over an existing journal is refused. Caller must retain this one
journal for the whole authorized run; it is not an adversarial billing service.
D3 requires an approved preregistration and current price recheck before
reading any credential; the v2 live run satisfied both. D2 mock prices/usages
remain **synthetic**, separate from the observed D3 usage and cost estimates.

## Verification and current state

See [machine-readable evidence](../../ops/formation-deepseek-d2-2026-09-28.json)
and [acceptance report](../../ops/formation-deepseek-d2-2026-09-28.md).

- D2-C implemented and locally verified; actual integration uses loopback
  mock model + real pinned Coordinator/D1 + Python Runtime/Verifier.
- D2-C #1427 and D3 controller #1431 are merged; no deployment is implied.
- D1/D2-A/D2-B merged; no remote migration or deployment in this task.
- [D3 v2 actual live acceptance](../../ops/formation-deepseek-d3-results.md):
  G0–G5 PASS, six calls, no retry; 5428 input / 276 output tokens,
  $0.001967 peak-equivalent estimate, $0.486612 reserved maximum.
  Actual account billing was not measured. Credential read occurred only after
  full preflight through the explicitly authorized limited injector.
- 5b is completed as bounded general-LLM Candidate Generation integration.
- #1421 untouched; 6a stays independent; no general efficacy or production-ready claim.
