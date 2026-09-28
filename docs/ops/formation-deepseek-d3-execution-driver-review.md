# D3-E execution controller — offline review, live BLOCKED

## State and exact snapshots

Preregistration #1429 merged at `c3f44c6e548cc4f73acaf37966621d21bad0097a`,
from exactly `381abac603a357ecf787b5ba4dd840b2dcf37b43`. No branch merge/rebase,
plan edit or protection-rule change was made. Normal merge was refused by branch
policy; the explicitly authorized exact-head admin merge was used.

- Controller base: `c3f44c6e548cc4f73acaf37966621d21bad0097a`.
- Tested controller implementation: `dcbf9ac5203cb39a632ad93d0c46aea045dd83c2`.
- Entry script SHA256: `2278ae6bc0ad39f44f377151c9f6c969e74edca6fd284335636240025f211d32`.
- Transitive controller bundle SHA256: `43b1f6147a45ba3b95e19f2790f27db0bbf4fc48be4ea8a14128942f8a4a1c83`.
  Algorithm: sorted repo-relative filename → SHA256 map; compact sorted JSON → SHA256.
- Immutable external plan SHA256: `b67607c6cff1de5339f79822bd3e5d6610d1663dcce6e3f0d6b418ef8a2a1585`.
- Execution ato: `10ad1cd8338e8261cc5bd5e944c38867f22bfe8b` (separate clean detached checkout).
- Execution API: `38668a97e7632256b33074b0d223670c16b76bfd` (separate clean pinned checkout).
- Receiver WASM: `d4732b46f1ce5ab3d1193d957d7bcb2d24f0469ffaa80e267c6cfda5bccb8a46`.

The plan bytes were extracted with `git show` from the #1429 merge into an
external artifact, not implicitly loaded from the controller's current same-name
file. Requester/Runtime/worker are built from the execution checkout, NEVER the
controller head. The two small controller helpers link that checkout's existing
Rust libraries. Their dependency versions must be a subset of its Cargo.lock
(with only the standalone control package added).

## Detected mismatch — blocking exact-request evidence

[The pinned requester](https://github.com/ato-run/ato/blob/10ad1cd8338e8261cc5bd5e944c38867f22bfe8b/apps/formation-worker/src/runtime_network/proposal.rs#L703-L746)
calculates remaining time **after** the successful durable claim, overwrites
`request.remaining_budget.timeout_ms`, then invokes `DeepSeekCandidateProducer`.
The pinned adapter serializes those bytes privately immediately before its
reservation/credential/send path. There is no observation hook for the exact
post-claim request.

The controller can reproduce the immutable context/K/catalog and the pinned
Rust requester's **preclaim** evidence projection, but its hash is NOT the exact
provider-visible hash. Reconstructing it externally with a different timestamp
would be false evidence. Neither a TLS/model proxy nor a second model client
is an acceptable workaround.

Consequently `execute` has an unconditional, explicit blocker **before journal
initialization and before G0**, not a late surprise after paid cells. The claim
proxy independently rejects missing exact-wire evidence too. No flag silently
relaxes this. The preregistered plan and execution pin remain unchanged.

**This is an offline-reviewable Draft, NOT an execution-ready/completion claim.**
Resolving the new exact-request recording requirement needs explicit approval
for a capture hook in a new implementation pin and a separately versioned plan,
or retaining the current pin and this blocker. The current plan is never edited.

## Control design

`deepseek-execute.py` has build / preflight / execute entry points. No Python
model HTTP client is present. The only public internet operation is one GET of
the fixed official pricing page, with redirects/proxies/cookies disabled,
30-second timeout and 1MiB cap. The other HTTP code targets literal loopback
Coordinator only. No provider response passes through the response-loss proxy.

A–M order is recorded: external plan hash; exact clean ato; exact clean API;
WASM; prompt; fixture hashes; production Rust projection reproduction; source
context hashes; current model/version; current peak prices/freshness; existing
Rust reservation arithmetic; absent first-run journal; executable hashes.
Existing/corrupt journals fail closed; there is no implicit reset/resume of
paid cells. G5's confirmed-completion restart is separately GET-only.

Fresh public fetch verified on `2026-09-28T14:16:46.586846+00:00`:
- final URL: https://api-docs.deepseek.com/quick_start/pricing/
- response SHA256: `210f102275ccf1a6542f08a3bc9e4b4c7c83278cb74b35217bffa112df6363b2`
- model/version: deepseek-flash / DeepSeek-V4.1-Flash
- peak cache-miss input $0.30/M; peak output $1.20/M

The parsed prices must not exceed registered caps; decreases do not increase
calls. Freshness <=1 hour. Only timestamp/URL/digest/model/version/parsed prices
are saved, not cookies or unrelated response headers.

`budget.rs` only parses BudgetPlan and calls its existing validate/create/reopen.
`CallBudget::create` is create_new + fsync. This helper has no provider, key,
source or K handling. One shared journal per run, no refund/delete/per-cell reset.
The 81,102 micros/call and 486,612 total are computed by existing Rust, not a new
Python cost authority. The live journal has **not** been initialized; helper
creation tests used a distinct offline fixture journal.

Only a child live Rust requester would inherit the controller environment;
Python never accesses or checks DEEPSEEK_API_KEY. Runtime/Coordinator receive a
small explicit environment map instead of a copy of all parent variables.
No .dev.vars read, key existence check, hash or print is performed.

## Cells and evidence

The orchestration loop consumes **plan.cell_order** unchanged. Each request uses
the registered frozen projection and the shared journal. Before each cell,
STOP/pending/failed protocol evidence prevents a new reservation. Unsupported,
validator rejection, invalid proposal JSON and actual K FAIL are ordinary cell
outcomes (no retry); provider/protocol/infrastructure violations stop the run.

- G0: one call, durable valid raw and admission; Verifier PASS not required.
- G1: no Preset/known D, admitted new D and accepted actual same-K receipt required.
- G2: before forwarding claim, require registered known D
  `sha256:a2490b6b6c84ad4242d81dc6afea6f04f82f041682389929a1baee6d543296cc`,
  actual claimed/finished FAIL, same K, execution attestation, verification-stage
  HTTP failure and receipt tied to that attempt. Missing/running/UNKNOWN/wrong-D/
  wrong-K/non-executed/unreceipted states reject before claim. No fabricated
  failure summary is injected; the helper invokes pinned `proposal_request_v2`.
  Its saved preclaim view excludes private source mapping/paths/raw logs; it
  truthfully marks exact provider wire SHA unavailable.
- G3: frozen K/policy; only known/admitted D attempts, no authority escape.
  Safe/unsupported/rejected can satisfy the safety gate.
- G4: bounded terminal outcome, no false verified route; unsupported not forced.
- G5: proxy forwards exactly one Coordinator completion, confirms commit with
  GET, drops only that response, then rejects any further claim/completion.
  Restart retains the same source/journal/config and satisfy ID; GET returns
  durable raw/provenance; pinned requester independently recompiles. One
  reservation and an actual accepted Verifier receipt remain required.

No D3 live results file is generated by preflight/tests. Future actual results
would record raw assistant bytes/hash, exact request hash, provider provenance,
usage, peak-price equivalent estimate, validator outcomes, D/attempt/Runtime/
receipts/K and terminal classification. Actual account billing remains **not
measured**; it must not be conflated with the estimate or reserved maximum.

## Offline verification (not live integration)

E0–E14: **all PASS**, 17 driver tests including extra A–M and key/client-boundary
checks; unchanged prereg tests8 PASS; total **25 PASS /0 FAIL**.

| Test | Negative/control |
|---|---|
| E0 | wrong plan; no journal/credential/call |
| E1 | wrong execution ato SHA |
| E2 | dirty execution checkout |
| E3 | wrong API SHA |
| E4 | wrong WASM |
| E5 | prompt drift |
| E6 | fixture drift |
| E7 | source-context drift |
| E8 | peak price increase |
| E9 | stale public price snapshot |
| E10 | existing/corrupt journal; bytes untouched |
| E11 | changed cell order |
| E12 | G2 actual-failure requirements (synthetic negative fixtures) |
| E13 | actual loopback fake-Coordinator commit/drop/GET, same-journal restart control, second claim rejected |
| E14 | STOP prevents next cell; no reset; capture blocker stops before G0 |

Real clean-pin Rust projection preflight A–M PASS; official public fetch PASS.
Pinned-library helper create/reopen PASS; duplicate create and corrupt reopen
reject. Helper Clippy -D warnings, rustfmt, diff check PASS. No production Rust
source/receiver/prompt/source-policy/compiler/K change; previous C2 actual tests
were not rerun or misrepresented as this driver's live integration.

**Live G0–G5: NOT RUN. Runtime/Verifier integration of D3-E: NOT RUN.**
Implemented control path / offline verified / Draft unmerged / not deployed.
**live calls=0; spend=$0; DEEPSEEK_API_KEY unread**. No remote migration, deploy,
feature flag, #1421 change or 5c work. Await the exact-request capture decision.
