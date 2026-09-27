# ADR-037 — Bounded typed Derivation generation (Formation 5b-a)

Status: implemented on a local/integration branch; not merged or deployed.
Fixed-draft execution and safety integration verified. Live Jev declined; the
LLM-generated Dnew PASS acceptance and the overall 5b completion gate remain open. This is a draft design record, not an accepted RFC. Builds on
ADR-034–036. Migration 0303 is additive; prior migrations stay immutable.

## Problem and value hypothesis

The 5a live pair chose the same known D as the default, with two attempts in
both arms. A single pair does not establish a general model-performance claim.
5b instead tests whether a model can produce a new typed recipe under explicit
authorization, then have the common Runtime and receipt authority establish
that it satisfies the same K. A generated draft, a new digest, and an HTTP 200
alone are not success. The acceptance target is D1 FAIL → bounded evidence →
typed draft → Dnew → actual execution → same K PASS.

## Audit and first slice

Existing reusable primitives are `parse_capsule_toml`, `authoring::bind`,
`BoundDerivation::derivation_ref`, `plan_candidate` / `lower_execution`,
Runtime capability admission, source tickets, common Runtime attempts, and
the shared Rust receipt authority. No shell generator or new executor is
needed. Jev returns typed choices, not arbitrary JSON/text generation; the
requester composes its validated field answers into the strict draft below.

The initial vocabulary has one operation, `python_script`, on a single
existing pinned Python process serving step. The owner explicitly authorizes
up to 16 opaque entrypoint IDs referring to immutable source files. The model
selects operation and entrypoint fields; it does not select from a precomputed
list of candidate DerivationRefs. Canonical Dnew is compiled after the answer.
This is bounded parameter synthesis, not open-ended program generation.

```json
{
  "schema": "ato.formation-derivation-draft/1",
  "operation": "python_script",
  "entrypoint_id": "entry_app"
}
```

Unknown fields, other operations and unauthorized IDs are refused. There is
no place for K, permissions, Runtime facts, argv, environment, URL, shell,
source patch, secret request or arbitrary Binding. The compiler takes the
parent D, pinned interpreter and source closure from the frozen owner scope,
replaces only the serving invocation with the authorized Python file, and
rebinds through the existing canonical authoring compiler. K must be exactly
unchanged. Unsupported parent routes, preparation steps, state, environment,
Bindings or network expansion are refused; supporting them needs a separate
vocabulary/admission review. Paths are private owner data: absolute, hidden
and traversal paths are disallowed, and the requester checks existence in the
recursive frozen source inventory; symlinks are not followed.

## Authority and persistence

`SearchPolicy.generation` is optional and frozen at creation. It names the
base DerivationRef, authorized entrypoint map, one-generation ceiling and
bounded timeout. Omitting it preserves prior search bytes and behavior.
The initial candidate vector never changes. A separate append-only generation
record adds at most one admitted candidate to the effective search frontier.

Generation opens only after safe known-candidate exhaustion with remaining
attempt/byte/deadline budget. Owner stop, unresolved UNKNOWN, in-flight work,
unaccepted receipts and effect-policy refusals retain priority. A provider
cannot treat UNKNOWN as failure or create another search to evade its budget.

Before model invocation, the requester claims the point durably. A second
requester or restart cannot invoke it again. A crash after claim without an
answer times out; there is no model retry. One completion records admitted,
duplicate, declined, invalid, provider_error or timeout. All outcomes spend the same
single generation allowance. Provider failure returns to the deterministic
frontier; it does not manufacture a K failure.

The Coordinator recompiles the draft using the bounded Rust WASM entrypoint,
checks canonical dedup against existing Ds, and stores the approved route and
provenance separately. Revision fences, one point per search, write-once
claim/completion, UNKNOWN/stop barriers and open-generation ticket fences
protect concurrent submissions. The ordinary scheduler/admission path then
places Dnew. The Runtime replans the source ticket and checks the expected D/K
before execution. The requester independently recompiles the recorded draft
before accepting any resulting receipt, including after restart. It pins the first
accepted generated D locally; later status cannot grow that authorization set.

## Model input, provenance and bounds

The model sees only fixed operation vocabulary, opaque entrypoint IDs and
bounded typed failure evidence at the 5a-b privacy boundary. It never receives
the scope's paths, source or manifest text, raw logs, receipt bodies, secrets,
unrelated Runtime facts or host identity. A separate projection constructs
the provider request; the durable generation row is not serialized wholesale.

Model pin, provider, prompt version, input/output usage and generation path
are provenance beside the draft/D, excluded from canonical D identity.
The decision and generation keys use separate environment names. A local
acceptance may bind the user-designated key to the generation process only;
this is not production provider configuration.

The first slice fixes max_generations=1, caps the point deadline at 30 seconds,
and reuses the bounded no-redirect Jev HTTP transport with no retry. Existing
max_decisions, attempt reservations, transfer/expanded/stored accounting and
search deadline remain independent limits. A generated D consumes an ordinary
attempt if executed; generation itself grants no execution permission. Source
archive cost is reconstructed from the stored request, including when no
Runtime placement is available; insufficient transfer budget terminates before
opening a generation point. An Exact Runtime constraint remains exact: opt-in
generation never permits execution on another Runtime. Without generation
authorization the old exact-search termination behavior remains unchanged.

## Completion and non-goals

The first slice needs actual same-K D1 FAIL → Dnew PASS, negative authority
tests, canonical dedup, restart reuse and concurrent generation fencing.
Performance claims require verified outcomes and measured comparisons; a
one-entrypoint fixture proves the integration boundary, not general repair
quality or superiority to a deterministic compiler.

Arbitrary shell/network, source changes, K weakening, permission growth,
UNKNOWN bypass, general new Adapter vocabulary, multi-service generation,
and unbounded repair are outside this slice. Probe remains deferred. Remote
migrations, deployment and feature activation remain separate work; new
clients must not be connected to a receiver lacking the required schema.

## First measurement, including the failed live target

On 2026-09-26 the fixed typed draft passed through the actual Coordinator and
Rust Runtime to a new D and a same-K verified receipt. A single live
`jev-1.13.0` call instead returned the valid `decline` / `none` combination.
It was durably declined, with one failed initial attempt and no generated D.
This is a safe outcome, but it does **not** meet the live new-D PASS target.
No live retry or prompt change was made to select a more favorable result.

That run exposed missing provenance on valid decline. The follow-up preserves
its strictly bounded model/prompt/usage metadata without changing D identity,
the prompt or generic error fallback. The first run's usage and full elapsed
time cannot be recovered and remain explicitly unknown, not zero. The
[ledger](../../ops/formation-typed-generation-2026-09-26.md) distinguishes the
fixed provider, live failure, and post-run observability hardening.
