# Formation D2-C acceptance — 2026-09-28

This is **mock-provider integration**, not live DeepSeek efficacy. Live calls
**0**, spend **$0**, no API key read. D3 G0–G5 and model preregistration have
not started. No D2-C merge/deploy, remote migration or #1421 change.

## Exact pins and scope

| Slice | PR | Base | Head | Merge |
|---|---|---|---|---|
| D1 | [API #701](https://github.com/ato-run/ato-api/pull/701) | `b81ef143caac3f8e478b0954e9a5db2932c99f19` | `5ab1b6e8c12f487ab727ae2e723e9e5aed4ffcc9` | `9fa4be45adf7dbda1fa68689c365a1806921569f` |
| D2-A | [ato #1426](https://github.com/ato-run/ato/pull/1426) | `d4fa39a693eb253949c65486288908e9ae47cf57` | `bf87487dadf981c192707ff3ed3966428ca2674e` | `c4bb53564669065aa6c47d1ed283e02ac6fa68ae` |
| D2-B | [API #703](https://github.com/ato-run/ato-api/pull/703) | `d4396175f384f7229edfcad8fc130d96cc2b0296` | `0b273085e2a2fc34dfcd050425c98646c4c405c7` | `38668a97e7632256b33074b0d223670c16b76bfd` |

D2-B branch started at D1 merge, then integrated concurrent main #702 before
repeating all 312 receiver tests. D2-C branch starts at D2-A merge. Actual
implementation/harness pin: `be6acd8a547ae4077035e2a551b3c0a78cc84bc4`.
Following documentation/ledger commits do not change tested runtime/harness.

Receiver pinned to **D2-B merge**, not moving main. Rust WASM source is D2-A
merge; SHA256 `d4732b46f1ce5ab3d1193d957d7bcb2d24f0469ffaa80e267c6cfda5bccb8a46`.
No D2 migration. Isolated Miniflare/D1 initialized from that pin's bootstrap
(includes 0304/0305 and independent main 0306); remote apply = none.

## M0–M13: all PASS

Actual requester, production authenticated Coordinator/D1/Rust rehydration,
ordinary Python process Runtime and actual HTTP Verifier ran on isolated
Linux/aarch64. Only model transport was replaced by a synthetic loopback HTTP
server. No caller-selected free-form executable/argv/path was generated.

| Case | Actual result |
|---|---|
| M0 valid raw | zero known D/no Preset → admitted first D → Runtime → Verifier → accepted |
| M1 empty content | malformed_response, 0 attempts |
| M2 malformed envelope | malformed_response, 0 attempts |
| M3 markdown/non-proposal raw | transport success; raw retained exactly; Ato invalid_output; 0 attempts |
| M4 >16KiB content | response_too_large, 0 attempts |
| M5 401 | provider_refused, 0 attempts |
| M6 429 | provider_refused, no retry, 0 attempts |
| M7 500 | provider_refused, no retry, 0 attempts |
| M8 connection failure | transport_error, 0 received HTTP requests, 0 attempts |
| M9 timeout | durable timeout/unknown usage, no retry, 0 attempts |
| M10 missing usage | malformed_response, 0 attempts |
| M11 injection-shaped source + unauthorized/K-mutation output | validator rejected, 0 attempts |
| M12 two valid proposals | exactly 2 admitted; finite DecisionProvider selects good D; actual receipt/acceptance |
| M13 lost completion response + requester restart | one model HTTP request total; durable raw reused/recompiled; actual receipt/acceptance |

14 producer reservations, **13 mock HTTP requests** (M8 cannot connect), 0 live
requests. All cases have the exact same frozen ContractRef:

`sha256:70d957ef2d15c61af1651a5ef9ae04468b286c815d29e71b3e7b8acdfe63e6ee`

M0 actual attempt `01M3KM6YCA60C4JSW3SGEDB04P`, D
`sha256:974ac4557bc3fb28fba976796f28910741feb8e72cd475eef5efaee057cd8f8f`.
Receipt `ato.contract-verification-receipt/1`: fully_satisfied=true,
GET `/health` HTTP 200, observed body SHA256
`e1facfb37c9f39db4aedb1c196f56a0b5c0f90b0fc2318977b1036d9e0aaf999`.
The full M0/M12/M13 receipts and exact evidence hashes are in the
[machine ledger](formation-deepseek-d2-2026-09-28.json).

## Source, transport and cost controls verified

- Captured request/2 contains authorized frozen excerpts and opaque IDs only.
  M0 context is 756 bytes against explicit 16KiB policy; hard ceilings remain
  64KiB aggregate / 8KiB per entry. Context and request hashes are in the ledger.
- Working-tree writes after capture do not change context; invalid UTF-8/binary
  omitted; deterministic UTF-8 truncation/module membership checked.
- No private map/path fields, unrelated files, Binding values, host identity,
  raw receipts/logs, API key or claim fence in provider payload. Synthetic key
  appears only in the required Authorization header, not request JSON.
- Provider raw error bodies and reasoning are absent from durable completion.
- Persisted provenance identity/usage tampering is rejected before admission.
- Fixed /1 remains source-free; source opt-in cannot be silently bypassed.
- Guard tests: fitting reservation, overbudget no send, exhausted second call
  no send, restart no fresh allowance, concurrent one winner, corrupt partial
  journal fail closed. Mock price/usage numbers are synthetic, not real spend.

Prompt `ato.formation-candidate-producer-prompt/1`, SHA256
`a3217b28b4242fdc03e11fe5dee7d91fbbc47b79358c4ae31601002b7a81ea50`.
Mock model `mock-deepseek-d2`, max output 2048, timeout 1000ms, thinking disabled.
No live model/default/pricing snapshot chosen; production config stays explicit.

## C2 fixed regression: all 15 groups PASS again

Same D2-C code and D2-B receiver, request/1 + fixed wire:

- zero-D and P0 unchanged no-policy behavior.
- P1/P2/P10/P11 known FAIL → producer once → generated selected PASS, same K,
  actual receipt required (receipt-removal negative check refuses acceptance).
- P3 wrong generated choice FAIL → durable evidence → next generated PASS.
- P4 unauthorized/K/shell rejected, execution 0; P5 2 valid + duplicate +
  unauthorized admits exactly 2.
- P6 error and timeout fabricate no K evidence; unsupported terminates bounded.
- P7 restart after claim, P8 concurrent one winner, P9 UNKNOWN barrier.
- L1 lost claim calls0; L2 lost completion calls1/restart reuse; L3 Coordinator
  restart preserves raw/D and requester independently recompiles.

**fixed calls=12**, model calls=0. Run `1790586462299575469`; mock run
`1790586337333674589`. Own processes stopped; temporary runner tokens removed.

## Local regressions / CI / limits

- Four C2-scope Rust packages: **654 PASS / 0 FAIL / 1 existing ignored**.
- Clippy all-targets, fmt and diff check: PASS.
- Receiver D2-B: **312 PASS** after current-base integration, typecheck PASS.
- Initial new mock tests found a privacy assertion matching the word “fences”
  in the prompt instead of an actual private fence, and a macOS accepted socket
  inheriting nonblocking behavior. Both harness issues were corrected without
  weakening authority assertions. Final full regression and actual runs passed.
- D2-A CI failures were exact-base matched/subset; CodeQL passed before its
  explicitly authorized exact-head admin merge. API D1/D2-B CI was blocked
  before execution by billing annotations, not a green run.
- D2-C is for review. No CI success is implied by local verification.

Implemented: yes. Locally verified: yes. Mock-provider actual integration:
yes. Live general-LLM integration: **no**. D2-C merged: **no**. Deployed: **no**.
The 5b general-LLM completion gate remains pending D3 preregistration/review.
