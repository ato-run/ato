# DeepSeek D3 preregistration boundary

D2-C #1427 is merged (`8980ab8aef353cd5e33dadce4a8dbf7367f4afa2`).
This follow-up does **not** authorize live execution. The first PR records inputs
and adds operational guards; no migration, receiver change or authority expansion.

## Immutable experiment versus observations

The JSON plan in `docs/ops/formation-deepseek-d3-plan.json` pins the implementation
commit, API commit, receiver WASM, prompt, provider/model/options, source files,
Rust-owned frozen K/D/catalog/context projection and six ordered cells. It does
not contain a model result, fabricated failure evidence or a verified route.
G2's real known-D failure is observed during execution, not synthesized at
preregistration. The source-context hash is independent of this later evidence.

Plan hashes are external SHA-256 values: a file cannot contain its own hash.
The implementation commit precedes the docs-only plan commit. Future execution
must check out the **implementation** commit clean, and load the approved plan
from an external path. It must not silently accept a newer docs head as the
execution SHA. Build the requester from that clean pinned checkout.

## Offline preflight (not execution)

`scripts/acceptance/proposal/deepseek-live.py` supports `generate` and `preflight`
only. Neither reads any environment credential or credential file; neither
contains a provider HTTP call. A wrong plan/SHA/cleanliness/WASM/prompt/model/
price/reservation/source/context pin fails closed. Projection uses production
Rust source freezing and authorization, before constructing any Coordinator
client. File hashes/manifests are transport evidence, not new semantic identity.

The first-run journal must be absent. An existing journal is **not** reset or
reused by preflight: recovery requires comparing durable Coordinator results
and is GET-only. A later reviewed execution driver must reuse the existing Rust
requester, initialize the exact budget once, and match each local projection
before claim. No independent Python model client or repair path may be added.
The acceptance example's explicit live option requires the pre-existing journal
and an exact expected projection; its ordinary fixed/mock defaults are unchanged.
The example alone is not the all-pin preflight or permission to execute.

Before any future execution, independently retrieve the official public pricing
page and recheck model availability. Preflight parses the supplied page's exact
peak/cache-miss rows, records its digest and requires a <=1 hour check timestamp.
It does not claim a local HTML file proves a fresh network retrieval. A price
increase, changed table or changed model version stops before credential read.
The journal is operational, not part of K, D, source closure or Capsule identity.

## Protocol and cost guard

- Exactly one choice, required model match, required finish_reason `stop`.
  Missing/length/other/empty content is malformed; never repair/retry.
- Required nonnegative integer usage, bounded by registered input/output caps.
  Cap violation is recorded and permanently stops the shared reservation journal.
- Raw assistant JSON text goes unchanged to the Ato validator. Markdown/prose
  stays invalid output (ordinary model outcome), not repaired JSON.
- Round/claim/completion/recompile/Verifier boundaries remain C1/C2's authority.
- At most six reservations, one per search/cell, no refund. An unresolved prior
  reservation blocks the next; rejected duplicate claims do not invoke a model.
- Separate upward rounding of input and output reservations yields 81,102 micros
  per call and 486,612 micros total, against the authorized 5,000,000 micros.
- Only closed finish categories, model-match boolean and observed usage enter
  the local journal. No reasoning, arbitrary error text, headers or credential.
- A failed protocol response halts further cells even after process restart.
  A successful model response is not a successful K verification.

## Gate and stopping point

G1/G2 require actual Runtime/Verifier same-K PASS; G0 protocol success; G3/G4
safety and boundedness (not forced unsupported); G5 one-call restart/raw durability
and actual PASS. All live results are currently **NOT RUN**. Mock tests cannot
close the 5b live gate. No 5b completion, general efficacy, production-readiness,
remote migration or deployment claim follows from this preregistration.
