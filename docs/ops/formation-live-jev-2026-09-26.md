# Formation 5a live Jev completion gate — 2026-09-26

**Implemented and locally/integration verified; 5a completion gate closed.**
5a-a and 5a-b are merged. No deployment, remote D1 migration, staging or
production change, feature flag, or persistent provider-key configuration was
performed. Probe remains deferred; 5b remains pending.

## Revisions and merge order

| Change | Fixed reviewed head | Merge commit |
|---|---|---|
| [ato-api #695](https://github.com/ato-run/ato-api/pull/695) | `c7b167267fcc86f881b980f27cd5e5d1895ab68e` | `1853f280753c2343f41e1c3e699b4cc6429317a6` |
| [ato #1410](https://github.com/ato-run/ato/pull/1410) | `c4ad819a6ea0c2ca35b390437dd67a58930e5534` | `c883087e6944ebf0cddc062bacf6e7206e2d5b23` |

API merged first. ato required an explicitly authorized administrator merge,
with `--match-head-commit` retaining the reviewed SHA. No manual CI rerun.
The acceptance follow-up is based on ato main
`c883087e6944ebf0cddc062bacf6e7206e2d5b23`; its driver/harness commit is
`ba04cd36d3d64251e3d3e5503ec16991bb8faab1`. Production provider code is unchanged.

## Method

Actual Coordinator (production Runtime Network routes, Miniflare local D1/R2,
0302 already applied to this isolated local database), Rust Runtime, Rust
`formation_search` requester on `oci-linux-test`. The new acceptance-only
`jev` mode calls the existing `JevDecisionProvider`, `serve_decision` and
Requester receipt authority without replacing them. The harness is
[`formation-live-jev.py`](../../scripts/acceptance/formation-live-jev.py).

The paired arms used byte-identical source, K and ordered candidate D set.
The harness asserted equality of fixture digest, ContractRef, candidate refs,
full frozen policy and numeric budget limits. Separate fresh owners prevent
the first arm's retained routes from bypassing the second arm's execution.
Opaque owner/Runtime/candidate IDs differ; permission and Runtime capabilities
do not. Runtime registration occurs before the measured interval; elapsed
time covers requester preparation, upload, decision, attempts and verification.

- Fixture file-tree digest: `dcfe7206bcbb4935773a6e12b2992f86e80934a7d0a99d74e716baa35a86fc3f`.
- K: `sha256:56339be22897221e23e0e97223fa6cb3bd33f6e583c4712dadaabe43d991c064`.
- D1: `sha256:37f4f60966bfd735ded38ce24a756b55a3310480ac8c807d1de21cc40cc76241` (no `/health`, known K failure).
- D2: `sha256:597f9cf4019345c7bfccb7cac61fb33ed3d0924acd87fd8f2be54ca462b66589` (Python notes fixture, PASS).
- Both: max_attempts **2**, max_decisions **1**, decision deadline **30 s**,
  search deadline **604800 s**, transfer/expanded/stored ceilings **10 GiB each**,
  mode `first_pass`, network `dependency-resolution`, managed Runtime disabled.
- The deterministic arm uses the local acceptance provider `fixed:0`; an
  assertion verifies that this is the core's recorded default. It has the same
  decision policy and spends one point, but makes **zero external model calls**.
  The no-policy behavior is separately covered by existing B0/A0 acceptance.
- The Jev arm has a 20-second HTTP timeout, no retries and at most one model
  call. After that point, the unchanged core runs any remaining default attempt.

The user-designated key in ato-api `.dev.vars` (`JEV_API_KEY`) was read without
printing it, passed through encrypted SSH stdin, and bound only to
`ATO_DECISION_JEV_API_KEY` for the acceptance requester process. It was not
written on the host, included in Coordinator/Runtime environments, committed
or persisted as configuration. Temporary owner token files were removed.

Authenticated `GET /v1/models` returned HTTP 200 and aliases `jev-latest` and
`jev-preview`. The [official model documentation](https://docs.typesafe.ai/models)
states versioned IDs are accepted even when the list contains only aliases.
The finite decision used pinned `jev-1.13.0`, confirmed in the live response.

## Result (one predeclared pair; no rerun or selection of a better result)

| Arm | Attempts | Elapsed | External provider calls | Input / output tokens | Estimated provider cost (USD) | Final outcome |
|---|---:|---:|---:|---:|---:|---|
| Deterministic default | 2 | 3.573 s | 0 | 0 / 0 | 0 | satisfied / verified |
| Jev DecisionProvider | 2 | 3.022 s | 1 | 1612 / 134 | 0.000067704 | satisfied / verified |

Jev selected offered label `c9fea2fcce74cf74c`: **Attempt D1**, also the default.
The Coordinator durably recorded `outcome=chosen`, seq=0, attempt_seq=0 and
the same model usage. Provider latency was **139 ms**, request size **3000
bytes**. Five offered labels included the two attempts, two candidate-refusal
inspections and Stop. No answer payload could supply an action or its arguments.

Both searches executed D1 FAIL then D2 PASS; both final receipts have
`fully_satisfied=true` for D2 and were accepted by the Rust requester.
Each spent exactly one decision, two attempts, 14336 transfer bytes, 4778
expanded bytes and 6656 stored bytes; all reservations settled to zero.
The complete paired ledger, including selected choices, usage, ceilings and
attempt identities, is [formation-live-jev-2026-09-26.json](formation-live-jev-2026-09-26.json).

Cost is an **estimate**, not a billing receipt: 1612 × $0.042 / 1,000,000,
with free output tokens, per the official model price checked 2026-09-26.
This pair shows successful bounded live selection and comparison, **not an
attempt-count improvement or statistically meaningful latency advantage**.
There is no evidence-based ranking advantage to claim from this fixture.

## Verification and remaining scope

- Requester example built with `cargo build --locked -p ato-formation-worker
  --example formation_search` on Linux.
- `cargo test --locked -p ato-formation-worker --test decision_provider_v1`:
  **7 passed** (HTTP failures, malformed/out-of-set answers, bounds/privacy).
- Harness syntax checked with `python3 -m py_compile`; paired actual
  acceptance exited 0, `ALL_PASSED` at **2026-09-26T11:26:04Z**.
- B0–B8, restart/evidence reuse, concurrent answer fencing and bounded privacy
  results remain in the [5a-b ledger](formation-5b-exploration-actions-2026-09-26.md).
  The broader implementation suites were not rerun for this acceptance-only
  driver/docs addition; their recorded results are not new live measurements.
- No Coordinator, requester or acceptance Runtime process remained after exit.

Executed artifacts (SHA-256):

| Artifact | Digest |
|---|---|
| Runtime CLI | `a02c962cb3a25b8e74be37e57ea66110b5c21b8b477a98b5ad19daa27bac86a6` |
| Rust requester example | `77b49c5d464b92ddda6d377933a95e6ea572283324104b3c230e2fa59b5a93b3` |
| Coordinator worker bundle | `22e3168c7f496e373c58d47ebaad28428c9b37c8fa788cf4819bb8f0cc42e767` |
| Rust receipt-authority WASM | `ceddfc605356dfaa275a2393aa866f0377ea7fe6dc7cb95a6db5d3c58e985ae3` |

0301 was not rewritten. Migration **0302** remains unapplied to remote D1,
as do 0299–0301 in this track. Any later staging rollout must apply
0299 → 0300 → 0301 → 0302, update API/Coordinator before connecting new ato,
and rerun B0–B8 plus 5a-a regression, DB fences and restart/evidence reuse.
Deployment verification is distinct from this completed local/integration
gate. Probe needs a safe typed primitive. 5b new-D generation has not started.
