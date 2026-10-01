# Exploration D-conversion diagnosis

Offline append to `formation-exploration-100.json`; no source, original result,
round limit, prompt, runtime or model was changed to produce this analysis.
Runtime `10473525efa26416ef63025dbb5003c910a43c70`, API
`b20bba7ab9daf282f8d5b1e22affe549ad3492a3`. All 1,205 original raw files match
`formation-exploration-100-raw-manifest.json`. Added model calls: **0**.

Reproduce with `scripts/acceptance/coverage/exploration-diagnostics.py --root
<verified-extracted-raw> --ledger docs/ops/formation-exploration-100.json
--manifest docs/ops/formation-exploration-100-raw-manifest.json --output
<new-analysis.json>`. The checked-in JSON omits proposal environment values;
the original hashed responses remain authoritative.

## 84 applications without a valid generated D

This is an exclusive partition by **last observed proposal disposition**, not
an attribution of root cause or model quality. Earlier dispositions and every
round are preserved in the JSON. There are 92 called apps, 8 with generated D,
3 of those unexecuted and 5 executed. Generated same-K PASS remains **0**.

| Last observed disposition | Apps |
|---|---:|
| Unexplained decline (`unsupported`) | 30 |
| Inspection budget exhausted | 14 |
| Inspection requested at final round | 9 |
| Unsupported entrypoint | 8 |
| Unauthorized inspection | 7 |
| Source OCI builder unavailable | 5 |
| Execution plan bounds | 3 |
| Unsupported dependency manifest | 3 |
| Proposal schema rejection | 2 |
| Typed schema violation | 1 |
| Source digest mismatch | 1 |
| Exploration ceiling exceeded | 1 |
| **Total** | **84** |

The 30 declines include supported-language Python/Node apps and unsupported
Go/PHP/Ruby/JVM/.NET workloads. Existing `unsupported` has no reason field.
Neither “bad LLM output” nor “correct proposal blocked by Ato” can be established
for these responses. This is a missing diagnostic, not evidence to change models.

Across **274 opened rounds** (273 CP calls), the dispositions were: 97 decline,
55 inspection requested, 29 inspection budget exhausted, 21 unauthorized
inspection, 14 unsupported entrypoint, 12 admitted proposals, 11 proposal schema,
9 unavailable OCI builder, 8 unsupported dependency manifest, 4 plan bounds,
4 typed schema violation, 3 invalid JSON, 2 digest mismatch, 2 unsupported OCI
selection, 1 duplicate requirements, 1 ceiling exceeded, 1 provider timeout.
The expired no-call round is consumed; it is not an additional model call.

## Round progress and evidence limits

38 rounds repeat an earlier semantic proposal after ignoring explanatory
`basis`/`unknowns` prose. 2 repeat canonical D and stop `no_progress` without
reexecuting it (WBO and Healthchecks). 22 rounds change execution fields, but
that alone does not prove a repair. No preceding validator error is followed by
a valid D under the conservative adjacent-round definition.

83 rounds request at least one previously unrequested source ref; 58 request
at least one ref already present in initial context or requested earlier. These
sets overlap. **Requested information is not proven received information.**
Original RequestEvidence stores transmitted request/body hashes and lengths,
not the provider-sent bodies. The actual text projected after the input budget
trim cannot be reconstructed from these hashes alone. Therefore this append
does not claim a count of rounds with new model-visible source information.

Code inspection identifies two additional information-loss boundaries:

- Source context after inspection replaces the initial context with the most
  recent inspected set. A later single-file request can drop the manifest and
  entrypoint previously provided. Only four initial files and bounded text are
  projected; repeated nested README files can consume the selected slots.
- Public catalog describes files only as manifest/readme/entrypoint/etc. A
  Cypress plugin and browser worker can be presented as entrypoints. Only 32
  public refs are listed even though private authorization may contain more.

These are hypotheses about causal impact; a changed-runtime pilot is required.
The private authorization map and host paths must remain provider-private.

## Valid D, not executed: 3 apps

| App | Actual stop | Ownership / implication |
|---|---|---|
| 6 SearXNG | Admission `authority_denied`: missing runtime bind for `ato.http@1 app.http` | Proposal omitted a declared-port requirement; Runtime enforcement is correct. Following round declines; no repair proposal. |
| 27 Wiki.js | Admission `ticket_unplannable`: unresolved Yarn version | Declared Node plan with no dependency step still consults source package-manager inference during lowering. This is distinct from provider/schema failure. No remaining round. |
| 49 OpenRefine | Admission `authority_denied`: missing HTTP bind | Proposal runs a Cypress test plugin under Node for a JVM service and explicitly says no HTTP serving path is known. Valid schema is not correct app startup. No remaining round. |

## Executed: 5 apps

| App | First concrete failure | Later-round change / outcome |
|---|---|---|
| 2 WBO | npm ci succeeds (146 packages); process exit 1, empty log tail, HTTP not observable | Same canonical D, prose changed, no reexecution. Exact launch cause remains unknown. This is not the historical OCI file-capability refusal. |
| 7 changedetection.io | Directly runs `changedetectionio/model/App.py`; `ModuleNotFoundError: changedetectionio` | Adds dependency retrieval, but second response violates schema; third selects unsupported manifest. Entrypoint stays unchanged. |
| 28 Kutt | npm ci succeeds (426 packages); `JWT_SECRET` is undefined | Inspection, then adds env including JWT_SECRET; validator rejects `execution_plan_bounds` because secret-like literal env is forbidden. An ephemeral binding is not currently representable. No production credential should be substituted. |
| 57 SVGOMG | Two npm ci operations, the second omits dev dependencies; `gulp` absent, exit 127 | Removes the second install; actual build succeeds twice. Both later D still start browser `src/js/prism-worker/index.js` under Node and fail named CommonJS export. Generated build-to-static serving is missing from execution_plan lowering. |
| 70 Healthchecks | pip `--require-hashes` fails: aiosmtpd==1.4.6 lacks hashes | Same canonical D, explanatory prose changes, no reexecution. Source-ref validation does not prove the entire requirements file is hash-complete. |

## Permission recovery denominators

Counts are unique apps within this search, based on actual refusal evidence,
matching requirements proposed in a later round, valid D, execution, and fresh
same-K PASS, respectively. Baseline-to-current reach is a separate comparison.

| Requirement | Shortage detected | Matching change proposed | Valid D | Reexecuted | Same-K recovery |
|---|---:|---:|---:|---:|---:|
| Network | 0 | 0 | 0 | 0 | 0 |
| Authority | 2 | 0 | 0 | 0 | 0 |

No executed app encountered a gate network refusal. Dependency-stage registry
access was allowed. Vaultwarden's proposal exceeds the frozen network ceiling;
it was rejected before execution and is a separate out-of-ceiling condition,
not a detected runtime network shortage. Authority shortages are SearXNG and
OpenRefine; neither produced a later matching repair. Network recovery efficacy
cannot be estimated from this cohort. Fixed-producer infrastructure successes
remain separate from upstream OSS evidence.

## Next bounded acceptance

Keep three rounds, model, frozen K, source pins and sandbox ceiling. First fix
diagnostic/projection loss and the existing adapter connection needed by an
executed representative; do not widen toolchains or authority merely to pass.
Capture the actual safe provider request before transmission in the next pilot.
Use a small preregistered upstream subset and record failure→repair→same-K PASS
separately from direct first-round PASS. A new 100-app run follows that gate.

Remaining conservative model reservation is **722,202 USD micros** before any
new calls. The previous 100-app worst-case reservation is **3,443,244 micros**;
another wave cannot be started under the current remainder. Pilot reservation
must be fixed first, and a full new wave needs a resolved budget gate.

Implemented diagnostics / measured existing raw / upstream repair **unverified**
/ merged **false** / deployed **false**. Historical ledgers are unchanged.
