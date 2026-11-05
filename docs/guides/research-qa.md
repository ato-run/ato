# Research Q&A

Status: Current technical reference.

Short answers to questions that come up when Ato is compared with test
frameworks, build systems and deployment tools. Each answer states what the
current tree does, and where the limit is. Detailed formats are in
[Contract and derivation formats](contract-and-derivation-format.md) and
[Sample K and D objects](contract-derivation-samples.md).

## 1. How is this different from Playwright or another E2E test library?

Playwright is a test framework. A test is an imperative program that drives a
running application and reports pass or fail. Ato's Contract K is declarative
data: a set of requirements that a realization must satisfy, canonicalised to
JSON and addressed by `ContractRef` (the SHA-256 of its canonical bytes).

The practical differences:

- **Test code versus content-addressed data.** A Playwright test can be edited
  to make a failure go away, and the edit is invisible unless reviewed. K is
  bytes; changing a requirement changes `ContractRef`. A receipt names
  the `contract_ref` it was verified against, and `ato reform` refuses a
  candidate that redefines K (`candidate_contract_mismatch`).
- **Where verification sits.** A test suite runs separately from the
  application. Ato verifies the Run that was just realized: `ato run` with
  `--verification-receipt` exits non-zero when `fully_satisfied` is false.
  Hosted import goes through validation, and a snapshot-bearing import requires
  the exact restored observation before it becomes Ready. This is an admission
  decision about that Run or import, not a general guarantee about every launch.
- **What is observed.** K is a closed list of requirements (see §6). A test
  suite can assert anything the language can express. K can only express what
  a verifier in the tree can decide. The two ecosystems can coexist: use
  Playwright or HTTP tools to explore an application, then encode the
  conditions you actually need as K.

Two limits matter here:

- The scenario verifier executes bounded **HTTP** steps; it does not embed a
  browser. `ato.browser@1` is a static-file serve step, not browser
  automation. Playwright appears in this repository only in verification
  records and archived operations notes as external tooling, not as part of the
  Ato verifier.
- K fixes the conditions, not the implementation. `reform` can verify a
  changed derivation under a frozen K. An agent that authors a **new** bundle
  with a different K is allowed to do so; the result is a different
  `ContractRef`, not a silent rewrite of the old one. Content addressing
  makes the change visible; it is not an access-control mechanism.

## 2. Do build failures belong in K?

No. K holds acceptance observations, not build steps or build logs. Building,
installing and launching are the Derivation's steps (or work that happened
before formation); K is what a verifier observes after a candidate exists.

- A build failure prevents the attempt from reaching verification. No
  `fully_satisfied` receipt is issued for that attempt, and the attempt
  outcome/evidence belongs to the attempt record, not to K.
- K is not rewritten to match the failure. A candidate that changes the
  requirements is refused before execution; a behavioral failure is a failed
  verification, not a weaker K.
- Source identity is a separate question. An `ato.contract.workspace@1`
  requirement binds an input's content reference, so a different source fails.
  A behavioral K omits it deliberately. Neither says anything about build
  reproducibility; Docker, Nix and scripts remain the tools for that.
- If the build output is what you care about, observe the output (an HTTP
  response, a data digest, a scenario), not the build log.

## 3. What happens when a derivation stalls?

There is no automatic fallback to another D. The caller selects a declared
Derivation explicitly, and a stalled attempt fails or is stopped rather than
silently retried on a different route.

The current mechanisms are bounded:

- **Scenario budgets.** A scenario has at most 64 steps, 60 s per step and
  600 s total, with at most 8 MiB per response and bounded headers, captures
  and JSON assertions (`lib/formation/src/scenario.rs`). Exceeding a
  budget fails the observation.
- **Launch and readiness bounds.** Runtime launch paths carry readiness
  timeouts and lease/attempt bounds; the local runtime terminates a worker's
  process tree with a grace period and escalates to SIGKILL, then reaps it
  (`apps/local-runtime/src/server.rs`). The portable Run terminates and
  reaps the process group on stop or drop
  (`apps/portable-application/src/portable_run.rs`).
- **No verified capsule from a partial attempt.** A Run that does not reach
  verification has no `fully_satisfied` receipt. Imported bundles pass
  through the existing quarantine/validation path; a Hosted state writer whose
  stop cannot be confirmed can be quarantined (recorded in the operational
  evidence for state recovery).
- **Effect classes gate execution.** Routes are classified as `pure`,
  `idempotent`, `record-substitutable`, `requires-confirmation` or
  `non-repeatable` (`lib/formation/src/authoring.rs`). The admission check
  refuses `requires-confirmation` and `non-repeatable` with `effect_policy` for
  every authorization variant in this build: a declared effect is a request, not
  an authorization, and no confirmed-authorization path for those classes is
  implemented yet. Only the first three classes are executed today
  (`lib/runtime-attempt/src/admission.rs`).

A correction to a common claim: this tree has no generic "UNKNOWN effect"
value that halts every retry. What exists is the effect-class gate above plus
writer fencing/quarantine for unconfirmed Hosted state. Arbitrary external
side effects are not magically classified; that is why non-repeatable and
confirmation classes exist, and why an uncertain stop needs recovery rather
than an automatic retry.

## 4. Engineer's assessment

Strengths:

- The separation of conditions (K) from realization (D) is real in the data
  model, not just in prose: K is content-addressed, D is selected explicitly,
  and the receipt names both. That is a cleaner boundary than a Dockerfile or
  a test suite provides.
- The admission split (bundle validation, host admission, execution,
  verification) keeps failure causes distinct. A missing runtime is not a
  tampered bundle, and a failed observation is not an admission bug.
- Frozen K plus receipt evidence is a useful governance primitive when an
  agent rewrites an implementation: the identity of the conditions is stable
  even though the code, D and bundle hash change.

Costs and risks:

- State is the hard part. K can bind declared saved resources, but it cannot
  cover arbitrary databases, process memory or external effects. A verifier
  ecosystem for real applications is the main open engineering problem.
- Verification on the start path adds latency and a new source of flakiness.
  Timeouts and readiness gates are bounded, but they still decide acceptance.
- The vocabulary and authoring surface are heavier than `docker run`.
  Familiarity is a real adoption cost, and presets cover only a small set of
  application shapes.
- The two identity profiles (M1–M4) are still an unresolved scope question.
  Until that is settled, documentation and tooling must keep the profiles
  distinct.

Where it fits: AI-agent sandboxes, long-running task handoff and
heterogeneous-runner dispatch — not a general replacement for Docker or a
test framework.

## 5. Can LLMs reduce the cost of tuning the essential behavior?

Substantially for drafting, less for deciding.

LLMs are good at: proposing K from existing tests, OpenAPI documents or
READMEs; probing a running app and filling in expected values; and repairing
D from build/admission errors. Ato gives that work a stable target because a
sealed K cannot be edited in place; a proposal becomes a new K with a new
`ContractRef`.

LLMs are weak at the judgment calls:

- **Over-specification.** Capturing a response body with timestamps, CSRF
  tokens, session IDs or random UUIDs into `body_digest` produces a K
  that can never be satisfied again.
- **Under-specification.** "GET / returned 200" can pass while the saved data
  or editability that mattered is gone.
- **Intent.** Whether undo history, caches or credentials must survive is a
  product decision, not a code fact.

The workable split is LLM proposal plus a human decision, then a frozen K and
automated D work. Note that `digest = "capture"` exists in the
`ato.capsule/1` grammar; the current `ato.capsule/2` authoring grammar
does not implement capture. Do not promise capture as a v2 feature.

## 6. How far can K go, what presets exist, and how much can it be tuned?

K currently supports five verifiers (details and limits in
[Contract and derivation formats](contract-and-derivation-format.md#6-what-k-can-express-today)):

| Verifier | What it can bind |
|---|---|
| `ato.contract.http@1` | status and/or whole-body digest for one method + path on a Port |
| `ato.contract.workspace@1` | exact source closure for an input |
| `ato.contract.scenario@1` | a bounded, ordered HTTP scenario with assertions and captures |
| `ato.contract.tls@1` | certificate name, validity, public-root chain and handshake |
| `ato.contract.instance-snapshot@1` | the exact restored saved-data snapshot |

Tuning, in practice:

- Omit `body_digest` for a status-only check; add it for exact bytes.
- Omit the workspace requirement for behavioral K; add it to bind the source.
- Use a scenario for ordered multi-step behavior instead of many HTTP checks.
- Add a snapshot requirement only for declared saved resources. Limits today:
  JSON data 1 MiB, browser state 16 MiB and 4096 items, filesystem state
  64 MiB, each Asset 50 MiB
  (`apps/portable-application/src/instance_snapshot.rs`).

Presets are a small, deterministic set in
[`lib/formation/src/preset.rs`](../../lib/formation/src/preset.rs):
`single-html/v1`, `static-files/v1`, `node-static/v1` and
`single-jsx/v1`. They identify authoring provenance and resolve to
stored semantics; changing a rule creates a new preset version. There is no
framework detection and no general "any application" preset. A project that
does not fit one of these shapes is told so rather than inferred.

## Claims this tree does not support

When discussing Ato, these are common overstatements:

- "Playwright is embedded in the scenario verifier." The scenario verifier
  executes HTTP steps; Playwright is not in the verification path.
- "K has a universal UNKNOWN effect that stops every retry." Effect classes
  and writer quarantine exist; a universal UNKNOWN class does not.
- "digest = capture works in ato.capsule/2." It is a v1-grammar feature.
- "Presets auto-detect frameworks and generate K." Presets are four fixed
  shapes with deterministic resolution.
- "K guarantees the app behaves the same everywhere." K guarantees only its
  declared observations, on that Run; see the limits section of the format
  guide.

## See also

- [Research tour](research-tour.md) and [Capsule Process Model](../theory/capsule-process-model.md).
- [Contract and derivation formats](contract-and-derivation-format.md).
- [State continuity](../concepts/state-continuity.md) and
  [compatibility](../concepts/compatibility.md).