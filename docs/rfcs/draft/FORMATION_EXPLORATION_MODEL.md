# Formation exploration model and phased plan

Status: Draft (non-normative).
Date: 2026-11-05.

This draft consolidates the exploration model behind Formation and a phased
plan for the optional decision and proposal layers. It does not change an
accepted RFC, does not add a semantic primitive, and does not commit to a
release date. It was checked against the current tree where implementation
status is stated; deployment is not claimed.

The user-facing model and current commands are in
[Formation](../../concepts/formation.md). This page covers the search model,
authority boundaries, phase status and open decisions.

## 1. What is being solved

Formation produces a candidate realization that reaches a continuation
satisfying a Contract, and records the evidence:

```text
I  --D on R-->  C  --Verifier-->  C satisfies K
```

`I` is the initial condition (workspace, source closure, existing objects).
`C` is the continuation reached on a runtime. A Capsule is the sealed,
addressable representation of such a point; K is the acceptance condition and D
is one declared way to realize it. Build, install, launch and probe are ordinary
steps inside D.

## 2. Authority boundaries

| Component | Owns | Does not own |
|---|---|---|
| Formation Worker | Interpreting source and author intent, forming K, managing and improving D candidates, inspection/probe, search budget and termination | A provider-specific scheduler or a second Runtime Network loop |
| Decision provider (optional) | Choosing among finite, Ato-offered choices; ranking; probe/stop decisions | Free-form execution, permissions, or deciding K |
| Candidate producer (optional) | Proposing typed new Ds, inspections or repairs when known candidates are insufficient | Directly executing a proposal or changing a frozen K |
| Deterministic Ato code | Schema/reference/type validation, policy and effect admission, canonicalization, digests and refs, sandbox, timeouts, fencing, publication, seal | Accepting a model's confidence as a verification result |
| Runtime Network | Runtime registration/capability/heartbeat, admissible D x R listing and ranking, reservation, attempt orchestration, evidence and release | Source interpretation or new D generation |
| Runner | Executing the exact D in an issued ticket and enforcing the execution boundary | Route changes or implicit fallback |
| Verifier | Comparing the realized continuation with K; returning observation evidence and a result | Judging correctness by a model score |

The split is intentional: Formation decides *which K and D to try*; the Runtime
Network decides *which permitted D x R can realize the frozen K now*.

## 3. Search loop

```text
Observe -> Generate -> Hard filter -> Decide -> Bind
   ^                                              |
   +---- Evidence <- Verify K <- Execute <---------+
```

1. **Observe.** Extract facts from I (package files, lockfiles, README) and
   from permitted runtimes. Collection is separate from execution; an active
   probe is itself admitted as an operation.
2. **Generate.** Start with presets, known Ds, cache and previously verified
   exact routes. Do not call a model for a case a deterministic preset resolves.
3. **Hard filter.** Match D requirements, runtime capability, verifier
   availability, bindings, authorization, data locality, effect policy,
   capacity and budget. Similarity or success probability never overrides a
   hard constraint.
4. **Decide.** The deterministic ranking is the baseline. When the optional
   decision layer is enabled, it selects one of the finite choices Ato offers;
   it cannot extend the set.
5. **Bind.** Ato revalidates and turns the choice into an exact attempt:
   D + I + concrete R + bindings + policy. After the attempt is issued, no AI
   decision rewrites that attempt's D. A repaired D is a new candidate and a
   new attempt.
6. **Execute, verify, record.** Execution, verification, external effects and
   cleanup are separate state axes. A complete verification PASS is a verified
   route; a failed or incomplete attempt is evidence, not a success.

The loop stops without weakening K or the runtime constraint when candidates or
budget are exhausted. It does not reset a global budget by starting a new
request, and it does not avoid an effect-policy stop by switching to another D.

## 4. K: formation, freeze and seal

- Before freeze, the source and author intent may produce a Contract template
  and a capture proposal. Canonicalization and freeze are done by Ato, not by
  the model.
- Once frozen, a failure is answered by changing D. K is not weakened, and
  expectations are not overwritten with values observed from the failed Run.
  A changed K is a new formation.
- Seal and run acceptance are separate. A seal decision may accept
  `Satisfied` plus explicitly deferred checks; a resume is accepted only when
  every required observation is `Satisfied` and the receipt says
  `fully_satisfied = true`.
- Formation receipts can rank candidates, but they cannot substitute for a
  complete verification of another execution. A receipt is tied to its D,
  runtime, bindings, policy, inputs and attempt.

## 5. Runtime constraints

| Constraint | Example | Handling |
|---|---|---|
| Exact binding | this laptop only | restrict to the exact runtime set; never fall back to another runtime |
| Hard constraint | local, CUDA available | search only runtimes that satisfy the predicate |
| Preference | local first, managed allowed | rank within the allowed set; never widen the allow set silently |

`require`, `allow` and `prefer` are distinct. Ownership, management, data
movement and fallback permission are policy, not capability. When no candidate
satisfies the constraint, the result says so; it does not move the work to a
managed runtime.

## 6. Phases and current status

The plan keeps each phase independently usable as a vertical slice. Status is
about the current tree; deployment is always separate.

| Phase | What becomes possible | Status |
|---|---|---|
| 1. Local Formation | Run and verify known Ds on one specified machine | Implemented: `ato form` local workflow and presets |
| 2. Runtime Network Formation | Search permitted heterogeneous runtimes for a route that satisfies K | Mechanisms are in the tree (client/server, `runtime-network`); deployment and acceptance are not claimed |
| 3. Decision layer | Improve selection among known candidates, inspection and probe choices | Mechanism is in the tree (ADR-035 decision provider; ADR-036 exploration actions); deployment and gate are not established |
| 4. Candidate generation | Propose new Ds from unknown sources or failures | Mechanism exists behind explicit policy and budget; not part of the normal path, and acceptance/deployment are not claimed |
| 5. Continuous adaptation | Increase verified coverage of an existing K as runtimes change | Not implemented; proposal |

### Phase completion conditions (summary)

- **Phase 1:** Static, Node and Python fixtures go from I + K + D + specified R
  to execution, K PASS/FAIL and per-attempt evidence. A failure on "this machine
  only" does not fall back to managed execution.
- **Phase 2:** Runtimes register and heartbeat; identity and capability profile
  are separate; the same K has a D x R that capability-matches but fails K, and
  another permitted candidate that passes. Offline/availability differences and
  evidence are recorded.
- **Phase 3:** The same request succeeds through both the deterministic and
  decision-provider paths; a provider timeout or error falls back to the
  deterministic default. A simple static case has identical canonical K/D with
  and without the provider.
- **Phase 4:** A small OSS fixture with no preset or known D goes through
  inspection, a new typed D, admission, execution and K PASS. Invalid
  operations, K weakening, unsafe retries and runtime deviations are refused.
- **Phase 5:** Under an explicit background policy and budget, a new runtime
  profile triggers selection of an existing K/I/D, execution, K PASS and
  coverage evidence. Adding a runtime does not itself grant data movement or
  execution permission.

## 7. Evidence and acceptance

| Aspect | Required evidence |
|---|---|
| Correctness | A capability match that fails K, and a different permitted candidate that fully passes |
| Identity | Frozen K stays unchanged; several Ds can share one K; equal K is not equal Capsule identity |
| Constraints | Exact R / require / allow are honored; prefer only ranks |
| AI boundary | Invalid proposals are refused; neither provider can bypass admission or the verifier |
| Retry safety | An uncertain external effect is not retried as if it were definitely safe |
| Availability | Offline, availability differences and provider failure are handled |
| Evidence | Each attempt records K, D, R, non-secret bindings, execution, verification, effect and cleanup |
| Seal vs resume | Deferred checks are recorded; seal eligibility and a fully satisfied resume are distinct |
| Budget | Attempts, cost and deadline limits hold across requests |

Fixtures, mocks, real executors, real formation, local runtimes and staging
hosted runtimes are recorded separately. A mock success is not an acceptance.

## 8. Open decisions

Before further implementation, these need an owner decision:

1. The canonical Capsule representation and how a formation result binds to the
   Capsule it produced, without inferring it from a Contract hash.
2. The API and state transitions from K template to freeze to seal.
3. The correspondence between Formation requests and Runtime Network requests,
   loop ownership, termination reasons and ranking hints.
4. Types for execution, verification, effect and cleanup, and the recovery
   procedure after an uncertain effect.
5. Budget allocation across jobs and network requests, including probe and AI
   cost.
6. Capability fact types and versions, and the separation between policy and
   availability.
7. Selection, authorization and duplicate-trigger handling for continuous
   adaptation.

## 9. Non-goals

- No generic agent framework, vector database, bandit or workflow engine before
  a phase needs it.
- No planner, capability inference or implicit fallback in the normal user
  path.
- No new semantic primitive: formation state and result are orchestration DTOs.
- No model-owned verification: the deterministic verifier remains the authority.

## See also

- [Formation](../../concepts/formation.md) and
  [Formation, verification and receipt](../../concepts/formation-verification.md).
- [Formation guide](../../guides/formation.md).
- [ADR-035 decision provider](ADR-035-formation-decision-provider.md) and
  [ADR-036 exploration actions](ADR-036-formation-exploration-actions.md).
