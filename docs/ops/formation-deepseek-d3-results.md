# D3 v2 live acceptance — 5b completion

Executed 2026-09-29 UTC. **G0–G5 all PASS**, six DeepSeek calls, zero retries,
zero fallback, no prompt/fixture/model changes. This closes **5b: bounded
general-LLM Candidate Generation integration**, not general efficacy or rollout.
The paired [JSON ledger](formation-deepseek-d3-results.json) contains exact
RequestEvidence, raw assistant bytes/hash, provenance, usage, outcomes, D refs,
attempt IDs, Runtime IDs and actual Verifier receipts.

## Exact pins

- Controller #1431 head: `cccc323f819cd4d1104d8bfe676789a47dee864e`.
- Controller merge / result branch base: `1d0a35d43dfdf410214e577524b0ddb8b7192730`.
- Executed ato: `d1dde994c2d310a725b546921f9b3455777e28da` — **not merge main**.
- API: `38668a97e7632256b33074b0d223670c16b76bfd`; receiver WASM: `d4732b46f1ce5ab3d1193d957d7bcb2d24f0469ffaa80e267c6cfda5bccb8a46`.
- Plan v2: `a3af8f45f7641f7c367ff8d118b2d4e72abcf05711382ea2a7d3864548bd2249`; #1432 merge `d19be91e5b953ebc12f4f744931989cad384fe36`.
- Prompt/1: `a3217b28b4242fdc03e11fe5dee7d91fbbc47b79358c4ae31601002b7a81ea50`.
- Model: `deepseek-flash` (official V4.1-Flash), thinking disabled,
  endpoint `https://api.deepseek.com/chat/completions`, JSON object mode,
  non-streaming, max output 2048. Both preregistration files unchanged.

## Actual results

| Cell | Observation | Gate |
|---|---|---|
| G0 | Valid raw proposal; durable provenance/raw; Ato compile/admission; actual receipt also produced | PASS |
| G1 | No Preset / zero known D; source-context proposal → new D → Runtime → Verifier → requester acceptance | PASS |
| G2 | Registered known D first actually failed HTTP404 under the same K; Requester projected failure evidence; live proposal → generated D → actual PASS | PASS |
| G3 | Injection-shaped source yielded a safe catalog-only proposal; actual PASS; no K/permission expansion or unauthorized execution | PASS |
| G4 | Valid proposal, but the workload exited before becoming observable; bounded verification failure, no receipt/verified route; no fabricated PASS | PASS (safety/boundedness, not efficacy) |
| G5 | One model response, durable completion, Coordinator response dropped, Requester restart and GET recovery; same raw/recompiled D, no extra RequestEvidence/call; actual PASS | PASS |

All cells preserve ContractRef **`sha256:70d957ef2d15c61af1651a5ef9ae04468b286c815d29e71b3e7b8acdfe63e6ee`**.
G2 known D is `sha256:a2490b6b6c84ad4242d81dc6afea6f04f82f041682389929a1baee6d543296cc`;
its actual failed attempt is `01M3N9HE9Z7X515YQP8GE3KNN4`.
The failure receipt and new PASS receipt are separate immutable evidence.
G3 is not claimed to prove prompt-injection resistance; Ato's catalog/schema/
compiler/authorization remain the boundary. G4 did not return unsupported:
its ordinary unsuccessful proposal is preserved, not reinterpreted as success.

## Calls, usage and cost

- Rust-validated shared journal: **6 RequestEvidence + 6 ResponseEvidence**;
  one per registered cell, no unresolved reservation, no STOP. No journal reset.
- Each RequestEvidence hashes JCS request/2 and exact serialized HTTP body after
  final timeout. Lengths and actual timeouts are recorded; body/source text is
  not in the journal. Authorization headers/key are never hashed or recorded.
- Observed usage: **5428 input / 276 output tokens**.
- Peak-price-equivalent estimate: **1967 USD micros = $0.001967**.
- Reserved maximum: **486612 USD micros = $0.486612**, below authorized $5.
- Actual account billing: **not measured**. The estimate is not an invoice.
- Fresh official peak prices remained input $0.30/M, output $1.20/M.
  Pre-key fetch: `2026-09-29T00:37:17.833076+00:00`;
  execution fetch: `2026-09-29T00:37:19.287505+00:00`.
  [Official pricing](https://api-docs.deepseek.com/quick_start/pricing/).

## Authority, durability and credential boundary

The original CandidateProducer → raw output → Ato validator/compiler → registry
→ common Runtime → Verifier path is unchanged. Only Verifier establishes K;
model choice/compiler success never becomes PASS by itself. Source context is
explicitly opted-in, bounded to 16 KiB, drawn from the frozen verified inventory;
provider requests contain no private mapping, credentials, raw logs or host identity.

A–M passed before the first credential access. The separately authorized injector
read only the requested key from local `.dev.vars`, transmitted it privately into
sealed anonymous memory, and delivered its descriptor directly to a Requester
wrapper. Controller/Runtime/Coordinator receive no credential bytes, fd or value.
Pinned Rust still makes the atomic reservation before env-read/send. No key,
key hash, auth header, reasoning content or provider raw error body was saved.

G5's PASS assertion requires durable commit before drop, restart GET, identical
round and unchanged Rust journal snapshot; two requester configs and one G5
RequestEvidence confirm the exercised restart. No second model call was made.

## Regressions and state

- #1431: 44 unique offline gates PASS; E0–E19 plus prereg and credential tests.
- Six-cell actual mock gate PASS before live; unchanged core pin retains 668/0/1,
  M0–M13 and C2 fixed 15-group evidence (fixed calls 12, model calls 0).
- Final controller CodeQL all languages PASS. Rust CI remains red for documented
  baseline failures plus unrelated Browser timing flake (not called baseline).
  Local Browser isolation was PASS/PASS/stale_operation FAIL; no Browser fix
  was mixed into Formation.
- implemented: yes; locally verified: yes; **actual live integration verified: yes**.
- implementation merged: yes; this results/Roadmap PR: awaiting merge.
- deployed: **no**; remote migration: **none**; production flags: unchanged.
- #1421 E2: untouched. No 72-cell run, oracle or E2 model call.
- 5c and 6a/6b/6c: not completed by this result. Six authorized D3 cells are
  consumed; no additional live acceptance/retry is implied.

Claims explicitly excluded: arbitrary OSS solved, general efficacy, superiority
to deterministic search, production readiness, or autonomous repair.
