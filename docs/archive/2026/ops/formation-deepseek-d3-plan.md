# Formation DeepSeek D3 — preregistration (NOT EXECUTED)

## State

- D2-C #1427 merged after exact-head admin approval; no protection rules changed.
- D3 guards and preregistration: implemented / locally verified; **not merged, not deployed**.
- D3 G0–G5 live Runtime/Verifier integration: **NOT RUN**. 5b live gate remains open.
- live calls = **0**; spend = **$0**; **API key not read**. No key existence check.
- 0304/0305 remote apply: none. No new migration. #1421 untouched.
- This PR contains **no live execution command**. A subsequent reviewed execution driver must consume this plan unchanged and reuse the Rust requester. No Python model client.

## Exact pins

- D2-C base: `f91cccd556cf2b7406eaabd68ae8d082ddfa59b1`.
- D2-C integrated/tested head: `c1f9feb28c760bc039c634998ce03c0bbc4d6179`.
- D2-C merge / D3 branch base: `8980ab8aef353cd5e33dadce4a8dbf7367f4afa2`.
- D3 implementation/execution pin: `10ad1cd8338e8261cc5bd5e944c38867f22bfe8b`.
- API pin: `38668a97e7632256b33074b0d223670c16b76bfd`.
- Receiver WASM source: `c4bb53564669065aa6c47d1ed283e02ac6fa68ae`.
- Receiver WASM SHA-256: `d4732b46f1ce5ab3d1193d957d7bcb2d24f0469ffaa80e267c6cfda5bccb8a46`.
- Plan SHA-256: `b67607c6cff1de5339f79822bd3e5d6610d1663dcce6e3f0d6b418ef8a2a1585`.
- Prompt `ato.formation-candidate-producer-prompt/1` SHA-256: `a3217b28b4242fdc03e11fe5dee7d91fbbc47b79358c4ae31601002b7a81ea50`.

The plan is a later **docs-only commit**. Check out the implementation pin clean,
build the example there, and load this plan externally with its approved hash.
Passing preflight does not grant live execution authorization.

## Protocol and spending

- Provider `deepseek`; exact model `deepseek-flash`; thinking explicitly `disabled`.
- Exact endpoint `https://api.deepseek.com/chat/completions`; redirects/retry/fallback disabled; stream=false; JSON object output; max_tokens=2048; timeout<=30s.
- Credentials are requester-only `DEEPSEEK_API_KEY`, read only after durable claim and reservation. The preregistration harness never reads environment credentials or credential files.
- Required response model match, exactly one choice, finish_reason=stop. Empty, length/other finish, missing usage or oversized envelope/content stops the run; no repair.
- Exact assistant text bytes enter the unchanged Ato validator. Invalid proposals/unsupported/K FAIL may advance the next cell; they are not retried.
- Usage beyond input/output cap persistently halts subsequent reservations; no refunds. Pending reservation blocks subsequent calls even after restart.
- Shared max calls **6**, at most one each in **G0 → G1 → G2 → G3 → G4 → G5**; max proposal rounds=1, max proposals=4.
- Hard reservations input **262144**, output **2048** tokens, not actual token estimates. Full canonical request + prompt + 1024-byte framing reserve is checked by Rust before key read.
- Public price/model check `2026-09-28T10:13:14Z`: peak cache-miss input **$0.30/M**, peak output **$1.20/M**. [Official pricing](https://api-docs.deepseek.com/quick_start/pricing/).
- Retrieved pricing page SHA-256 `210f102275ccf1a6542f08a3bc9e4b4c7c83278cb74b35217bffa112df6363b2`.
- Separate ceiling: input **78644** + output **2458** = **81102 micros/call**; total **486612 micros ($0.486612)** <= **5000000 micros ($5)**.
- Re-fetch official pricing/model availability immediately before any future execution. Price increase/table drift -> 0 calls. Offline preflight checks the supplied snapshot and freshness, not a fictional network recheck.

## Frozen fixtures and source policy

Owner-authorized regular files from a verified frozen archive only; entrypoint opaque IDs `e_17`/`e_42`. No mutable working tree reads by the provider, README/manifest allowlist, private mapping, paths as generated arguments, credentials or unrelated source.
UTF-8 only; max source text 16384 bytes, per entry 8192, core ceiling 65536. Injection-shaped comments in G3 remain untrusted application data. Ato validation—not the prompt—enforces authority.

| Cell | source manifest SHA-256 | source-context SHA-256 | source text bytes | known D |
|---|---|---|---:|---:|
| G0 | `b728a2224e892476417660be71520ca2e2f49aa3dde8912bb15208de0986064a` | `sha256:cb63ed5daf77b7b156278f65f9d01da6ab48369e3a0821bdb897bfeb4e197e42` | 438 | 0 |
| G1 | `b728a2224e892476417660be71520ca2e2f49aa3dde8912bb15208de0986064a` | `sha256:2b6aad1183a8338946f1d06869a9a230c4e86f23cc4f4eb05cc49e63dd2f5676` | 756 | 0 |
| G2 | `a1c5f4c7308c337f63d81638024efb4c2df38d173565cf182fbce8c2aa4f7ec8` | `sha256:2b6aad1183a8338946f1d06869a9a230c4e86f23cc4f4eb05cc49e63dd2f5676` | 756 | 1 |
| G3 | `8dd67e78f5949507b5aa721faf1d3d277c1545cad1eb8288b04b790ea407d5df` | `sha256:0c282a337ce2a672b684408145618333f22dcc4a021bb66f3414a14a93034738` | 454 | 0 |
| G4 | `8c7ffa6667ec3a090fba456b3d29e98ffd9252d3adf7a0a5ba83ecd4f6ec47b9` | `sha256:b8e7d89eaa04a37e5160639685020f6b357c83b49c3b08c84964ab2641da7433` | 128 | 0 |
| G5 | `b728a2224e892476417660be71520ca2e2f49aa3dde8912bb15208de0986064a` | `sha256:2b6aad1183a8338946f1d06869a9a230c4e86f23cc4f4eb05cc49e63dd2f5676` | 756 | 0 |

Full per-file hashes, source bytes via checked-in fixture paths, initial_source, K, known DerivationRefs, owner authorization, OperationCatalog and Runtime configuration are embedded in the JSON. Source hashes are transport evidence, not D/K identity.
All cells freeze ContractRef `sha256:70d957ef2d15c61af1651a5ef9ae04468b286c815d29e71b3e7b8acdfe63e6ee`.
G2 failure evidence cannot be pre-recorded: it must come from its real known-D attempt. The context/K/catalog are already pinned; the request includes the resulting bounded closed evidence.

| Cell | Registered criterion | Live result |
|---|---|---|
| G0 | Valid exact raw proposal; durable provenance; Ato compile; no authority escape. | NOT RUN |
| G1 | No Preset/known D; source-selected new D; actual Runtime/Verifier same-K PASS required. | NOT RUN |
| G2 | Known D actual FAIL before call; projected failure evidence; new D actual same-K PASS required. | NOT RUN |
| G3 | No K/permission escape or unauthorized execution; safe, unsupported and rejected outputs allowed. | NOT RUN |
| G4 | Bounded termination; no fabricated K evidence or false verified route; unsupported not mandatory. | NOT RUN |
| G5 | Completion commits then response drops; requester restarts; raw recompilation; total calls=1; actual PASS. | NOT RUN |

## Verification

- D2-C integrated head: M0–M13 all PASS; C2 fixed 15 groups all PASS, fixed calls=12/model calls=0; selected Rust **654/0/1**, receiver **312/0**, Clippy/fmt/diff PASS.
- Integrated-head CI: CodeQL PASS; Rust failures matched exact current base on each OS. CLI Windows/macOS matched current-main Rust signatures; no identical main CLI workflow existed. Overall CI was red, not described as green. No new connected-worker/D2-C regression. See [D2 ledger](formation-deepseek-d2-final-integration.json).
- D3 implementation pin: selected Rust **658 PASS / 0 FAIL / 1 existing ignored**, Clippy/fmt/diff PASS; 8 offline Python tests PASS; real Rust offline freeze independently reproduced all six projections.
- Clean exact ato/API + exact WASM + fresh public-price snapshot + fixture/context preflight PASS. Fourteen injected plan/pin/price/journal/context/order failures rejected, 0 calls.
- Added mock HTTP coverage: length/other/missing finish, wrong/missing model, input/output over-cap; persistent next-cell stop after restart. Existing claim-loss/completion-loss mock tests remain PASS.
- D3 actual Runtime/Verifier/live tests have **not** run; no new receipt is claimed here. The D2 actual receipts remain evidence for D2, not live DeepSeek efficacy.
- G5 future execution must drop only the successful completion response, restart Requester, GET and independently recompile durable raw; no model recall. A previous journal is rejected by first-run preflight and requires GET-only recovery review.

## Offline command

```sh
python3 scripts/acceptance/proposal/deepseek-live.py preflight \
  --binary /absolute/pinned-ato/target/debug/examples/proposal_search \
  --scratch /absolute/pinned-ato/.tmp/new-preflight \
  --plan /absolute/external/formation-deepseek-d3-plan.json \
  --plan-sha256 b67607c6cff1de5339f79822bd3e5d6610d1663dcce6e3f0d6b418ef8a2a1585 \
  --api /absolute/pinned-ato-api \
  --wasm /absolute/pinned-ato-api/src/services/runtime_network/wasm/receipt_authority.wasm \
  --journal /absolute/pinned-ato/.tmp/new-live-calls.jsonl \
  --price-html /absolute/fresh-official-pricing.html --checked-at <UTC-timestamp>
```

This command does not create the live journal, load `.dev.vars`, read a key or send a model request. Stop here for review. New remote rollout/feature flags and general efficacy claims remain out of scope.
