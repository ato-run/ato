# Formation common attempt (ato#1390) — Runtime Network acceptance (2026-09-24)

One run of each path through the Runtime Network after the common attempt
entry and receipt-authority changes:

```text
ticket → run_attempt (admission → start record → execute D → HTTP observation
       → C ⊨ K → receipt) → coordinator → requester accept_verified_route
```

## Setup

| | |
|---|---|
| Ato | `refactor/formation-common-attempt` (ato#1390), `ato` built from the branch on the Mac and on OCI |
| Coordinator | ato-api `feat/runtime-network-route-attempt-id` (ato-api#683: the route view names `attempt_id`) under `wrangler dev --local` on the Mac; local D1 with all migrations applied locally; one local account, two `user_managed` runner rows. No staging or production resource was touched. |
| Runtimes | `rt_e2e_mac` — macOS 15 / Apple M1, no containment. `rt_e2e_oci` — Ubuntu 24.04 aarch64, bwrap + Landlock, `/opt/ato/toolchains`, contained browser verifier (Stagehand, judge `jev-1.13.0`), reaching the coordinator over `ssh -R` |
| Requester | The Mac, with the Mac Runtime's token (acts for the owner) |

## Results

| Case | Fixture / options | Runtime | Result |
|---|---|---|---|
| 1 | `static-page`, `--exact-runtime rt_e2e_mac` | Mac | satisfied; K `480f2e7a…` = base; D `b4feb089…`; the route names attempt `01M399A6WGT0…` (environment `native`); receipts `http_contract` + `contract_verification`; the requester accepted it (exit 0) |
| 2 | `notes`, `--network dependency-resolution`, `--exact-runtime rt_e2e_oci` | OCI | satisfied; K `22d8402e…` = base; D `3a4c35b6…`; process candidate built, realized under bwrap, observed over HTTP; `execution_started = true` from the start record; the requester accepted it |
| 3 | as 2, plus `--verify-browser --accept "Create a note named 'formation-check'. Reload the page and verify that the note is still present."` | OCI | satisfied; effective K `e7247bd4…` over base `22d8402e…`; the route's `effective_contract_ref` equals the request's K; the browser receipt is `pass`, contained, and its `target.attempt_id` is the route's attempt `01M399CZFHVY…`; the requester accepted it only with both receipts |

Satisfy ids: 1 `01M399A6W8GCM10A97HHDPY7GP`, 2 `01M399CMY8WZ63BESJZACR60BC`,
3 `01M399CZFAJCDVMXDJM9H8JCJ1`.

The refusal cases (failing receipt under a pass label, missing / failing /
inconclusive / other-attempt / other-Contract / uncontained browser receipt,
different effective K, another environment's attempt) are covered by
`apps/formation-worker/tests/runtime_network_v1.rs` against coordinator-shaped
status JSON; they were not replayed through a tampering proxy here.

## Not covered

- x86_64 Runtimes, several environments on one Runtime, and a coordinator
  without ato-api#683 (the requester's `(D, Runtime, environment)` fallback
  is covered by tests only).
- Staging and production.
