# Formation

Status: Current technical reference.

Formation turns an explicit source or authoring input into a candidate
realization with a bound Contract K and Derivation D, executes it, and records
whether the result satisfies K. It is not a new semantic primitive: building,
installing and launching are ordinary steps of a Derivation, and verification
compares the realized computation with the unchanged K.

## Inputs and outputs

```text
I  source closure / authoring input
K  frozen Contract, or an authoring template to propose one
D  declared candidate realizations
R  runtime constraints (host, platform, capability, policy)
        |
        v
D runs on R -> C -> Verifier observes C against K -> receipt
        |
        v
sealed point / portable bundle, or a recorded failure
```

A successful build is not acceptance. The verifier is the only component that
decides whether K holds; capability match, a rendered page or an exit code of a
build step is not a verification result.

## Current entry points

- `ato pack <source> -o app.capsule` compiles `ato.capsule/2` authored input
  into a portable-application bundle. The authored Contract and routes become
  canonical `ato.contract/1` and `ato.derivation/1` objects.
- `ato form <source> --runtime local ...` runs the local source-candidate
  workflow described in the [Formation guide](../guides/formation.md). It is a
  separate workflow from `pack`, not a different verification authority.
- Hosted source Formation and hosted bundle intake are separate service paths.
  Their presence in the tree does not establish deployment or availability.

## Presets

The tree knows four App presets
([`lib/formation/src/preset.rs`](../../lib/formation/src/preset.rs)):
`single-html/v1`, `static-files/v1`, `node-static/v1` and
`single-jsx/v1`. A preset is authoring provenance that resolves to stored
semantics; a changed rule is a new preset version. There is no framework
detection and no general "any application" preset. A project that does not fit
one of these shapes is told so rather than inferred.

## The search loop

The formation search model is:

```text
Observe -> Generate -> Hard filter -> Decide -> Bind
   ^                                              |
   +---- Evidence <- Verify K <- Execute <---------+
```

- **Observe.** Collect facts from the input and the permitted runtimes; keep
  collection separate from execution.
- **Generate.** Start from presets, known Ds and cached routes. Do not call a
  model for a case a deterministic preset already resolves.
- **Hard filter.** Check D requirements, runtime capability, verifier
  availability, bindings, authorization, data locality, effect policy, capacity
  and budget. Ranking never overrides a hard constraint.
- **Decide.** The deterministic default is the baseline. The optional decision
  provider (Jev) can only choose among the finite choices Ato offers.
- **Bind.** Ato revalidates the choice and turns it into an exact attempt for
  one D, one runtime, bindings and policy. After the attempt is issued, no AI
  decision rewrites that attempt's D.
- **Execute, verify, record.** Execution, verification, external effects and
  cleanup are recorded as separate states. A PASS is a verified attempt, not a
  substitute for a receipt.

The deterministic path is the normal path. The decision-provider and
LLM-proposal mechanisms are present in the tree behind explicit policy and
budget; they cannot add a D, change K, grant permissions or bypass admission.
Continuous adaptation is not implemented.

## Authority boundaries

| Component | Owns | Does not own |
|---|---|---|
| Formation Worker | Search state, candidate and evidence management, budget and termination | A second scheduler or a second verification decision |
| Decision provider (optional) | Choosing one offered choice, ranking, probe/stop decisions within the offered set | New Ds, permissions, K, or execution |
| Candidate producer (optional) | Typed proposals for inspection or new Ds, constrained by policy and budget | Executing a proposal, changing K, or bypassing validation |
| Deterministic Ato code | Schema/reference validation, admission, canonicalization, digests, sandbox, timeouts, publication, seal | Treating a model's confidence as a verification result |
| Runtime Network | Runtime facts, admissible D x R listing, reservation and attempt orchestration | Source interpretation or new D generation |
| Runner | Exact D execution under an issued ticket | Changing routes or implicit fallback |
| Verifier | Comparing the realized computation with K and returning evidence | Judging correctness from an ML score |

## Invariants

1. Only the verifier decides that K holds.
2. A frozen K is not weakened. A failure changes D, not the observations or
   expected values.
3. A user runtime constraint is a constraint: an "only this machine" request
   must not fall back to another runtime.
4. AI output is a proposal. Ato validates permissions, policy, effect class,
   types, references and budget before execution.
5. Attempt evidence is per attempt: success, failure, deferred results and
   cleanup stay attached to that attempt.
6. D describes requirements; a concrete runtime is bound at execution time, not
   baked into D.
7. Do not duplicate fallback control between Formation and Runtime Network.
8. Secrets are rebound as runtime inputs; they do not enter identity, evidence
   or stored model input.
9. Build, install, launch and probe are ordinary steps, not new semantic
   primitives.

## K: proposal, freeze, verification and seal

- Before freeze, a formation may propose a Contract from source and author
  intent. Canonicalization and freeze are Ato's authority, not the model's.
- After freeze, a failure is answered by changing D, never by relaxing K or
  recapturing expectations from the failed Run.
- A different K is a different formation, not a repair of the old one.
- Seal and run acceptance are separate. A seal decision may accept
  `Satisfied` plus explicitly deferred runtime checks; a Run is accepted only
  when every required observation is `Satisfied` and the receipt reports
  `fully_satisfied = true`.
- A receipt from one attempt is ranking evidence for that attempt, not proof
  that another runtime or a later attempt will pass.

## Runtime constraints

A runtime constraint can be an exact binding ("this Mac"), a hard constraint
("local, CUDA available") or a preference ("local first, managed allowed").
`require`, `allow` and `prefer` are different things; ownership,
management, data movement and fallback permission are policy, not capability.
When no candidate satisfies the constraint, formation reports that result and
does not silently move to a managed runtime.

## Current status and limits

- Local formation (`ato form`) and `ato pack` are implemented. Presets cover a
  small set of application shapes.
- Runtime Network formation code is present in the tree; deployment and
  acceptance are separate and not claimed here.
- The decision-provider and candidate-producer paths exist behind explicit
  policy and budget. They are not part of `ato run`, and a normal run never
  invokes them implicitly.
- Continuous adaptation is not implemented.
- There is no universal builder. Build reproducibility remains the job of
  Docker, Nix, scripts or an OCI derivation; K does not contain build logs, and
  a build failure never becomes a K condition.
- General cross-host state restoration remains limited; see
  [state continuity](state-continuity.md).

The phased exploration model, acceptance evidence and open decisions are in the
[Formation exploration draft](../rfcs/draft/FORMATION_EXPLORATION_MODEL.md).

## See also

- [Formation, verification and receipt](formation-verification.md)
- [Formation guide](../guides/formation.md)
- [Application contracts](application-contracts.md)
- [Compatibility](compatibility.md)
