# ADR-021 — Runtime Network Phase 1: satisfying one K across owned Runtimes

**Status**: proposed
**Context**: Runtime Network Phase 1, on top of ADR-019 (ephemeral verification)
and ADR-020 (Browser Contract v0). Coordinator: `ato-api`
`src/services/runtime_network/`, migration `0286_runtime_network.sql`.

## The question

`ato form --runtime local` realizes and verifies a candidate on the machine it
runs on. A person owns more than one machine — an x86_64 Linux box, an ARM64
Linux VM, a Mac — and a route verified on one says nothing about another.
Which of them can satisfy a given K with a given D, and can that be decided
by running and verifying there rather than by assuming?

## The decision

```text
SatisfyRequest { contract_ref, authorized_derivations[], runtime_constraint,
                 bindings, policy, budget }
  → candidates   = authorized D × advertised execution environments
  → hard filter  (every refusal kept as a typed reason)
  → deterministic rank
  → AttemptTicket (fixed D, fixed R) → Runtime executes and verifies it
  → pass           → VerifiedRoute
    fail           → next candidate, only when fallback is safe
    inconclusive   → recorded, never a route
```

### Runtime model

- **Descriptor and availability are separate.** `PUT /runtimes/self` stores
  what a Runtime *is* (`runtime_environments`: facts, `facts_ref`, agent
  version). `POST /runtimes/self/availability` stores whether it can take work
  *now* (`runtime_availability`: capacity, current slots, health, observed
  time). A Runtime that stops reporting for 60 s is offline: it is not a
  candidate, but its identity, its facts and the routes verified on it stay.
- **Identity is the existing runner device.** The Runtime is a
  `runner_devices` row and authenticates with its runner token
  (`findRunnerByToken` + audience check). A `user_managed` Runtime acts for its
  owner. No second registry, no second credential.
- **Capabilities are open typed facts.** `platform.os`, `platform.arch`,
  `cpu.vendor`, `containment`, `runtime.process`, `runtime.browser`,
  `toolchain.<name>.<version>` … are measured by the worker
  (`probe_facts`) and matched as strings. There is no capability enum in the
  coordinator; a new fact needs no migration.
- **Execution environment ≠ physical Runtime.** Candidates are
  D × `(runtime_id, environment_id)`. Phase 1 workers advertise one
  environment, `native`; the key already admits more (a container
  environment, a VM) without changing the physical identity.

### Request and candidates

- **Only authorized Derivations.** The requester sends the authored routes
  (`capsule.toml` text) it authorizes, each with its `derivation_ref`, effect
  class, requirements and provisions computed in Rust from the planned
  Derivation. No preset, no LLM, no Jev generates a D here. Every route must
  bind to the same Contract: one request is one K.
- **Identities are computed once, in Rust.** The coordinator never parses a
  Derivation or recomputes a ref; it carries them. `contract_ref` and
  `derivation_ref` depend on the draft and the source closure only, so the
  same D has the same ref on x86_64 and ARM64. A platform restriction is part
  of D (`[[platform]] os/arch`), never of K.
- **Hard filter with evidence.** Every applicable reason is collected, not the
  first: `not_authorized`, `management_policy`, `runtime_revoked`,
  `runtime_drained`, `exact_runtime_mismatch`, `runtime_offline`,
  `runtime_unhealthy`, `capacity_exhausted`, `requirement_unmet {fact,
  expected, actual}`, `provision_needs_network`, `verifier_unavailable`,
  `binding_unavailable`, `effect_policy`. Rejected candidates are stored with
  their reasons (`satisfy_candidates.filter_reasons_json`).
- **Deterministic rank.** Exact Runtime → a route already verified for this
  K → fewer toolchains to provision → lower load → stable ids. A rank is a
  guess about what to try first, never a verdict.

### Execution

- **Fixed ticket.** `satisfy_attempts.ticket_json` is written once, with the
  D text, the Runtime, the environment, the archive digest and the browser
  Contract. The worker executes exactly that ticket; if its own planning of
  the ticket's route does not reproduce the ticket's refs, the attempt is
  `inconclusive` (`ticket_mismatch`), never a pass under another identity.
  The coordinator likewise refuses a reported pass whose refs differ from the
  ticket (recorded as `inconclusive` with a `coordinator_note`).
- **Fenced claim.** `POST /attempts/claim` increments the fence; a result is
  accepted only from the claiming Runtime with the current fence. An attempt
  expires — counted as a non-pass, so the request moves on — when it was
  claimed and reported nothing for 30 min, or when it is still unclaimed and
  its Runtime went offline or 30 min have passed.
- **Existing execution machinery.** The worker (`ato runtime-network serve`)
  runs the ticket through the local Formation driver with an `Archive`
  Initial Condition: the same freeze, contained build, temporary realization,
  HTTP Contract and Browser Contract v0 as `--runtime local`, with
  `max_attempts = 1`. The realization is destroyed afterwards. A verification
  attempt is not a user Run: it creates no `runs`, no `runner_leases`, no
  session.
- **Tickets are not leases or formation jobs.** `runner_leases` are bound to
  a user Run and its lifecycle; `formation_jobs` are hosted build/publish jobs
  with their own worker pool. A verification ticket has neither lifecycle, so
  it gets its own table with the same fenced-claim pattern.

### Outcome

- **VerifiedRoute** = `{effective_contract_ref, derivation_ref, runtime_id,
  execution_environment_id, capability_profile_ref, verifier_receipts[],
  verified_at}`, one per passing attempt. `capability_profile_ref` is the
  environment's `facts_ref` at issue time. A VerifiedRoute is evidence that
  this D on this R once satisfied this K; it is not part of K's identity, and
  several routes for one K are expected.
- **INCONCLUSIVE never becomes a route.** Only a `pass` whose refs match the
  ticket is written to `verified_routes`.
- **Fallback.** After a `fail` or `inconclusive`, the next candidate is issued
  only when the D's effect class is disposable (`pure`, `idempotent`,
  `record-substitutable`) *and* the request did not pin one Runtime. Under an
  exact constraint the request ends `unsatisfied` after that Runtime's
  attempt. Every attempt, failed or not, stays in the history.
- **Budget.** `first_pass` stops at the first route; `all` tries every
  admissible candidate within `max_attempts`.

### Surfaces

- `ato form <dir> --runtime local` is unchanged.
- `ato form <dir> --runtime-network --api … --token-file … [--route …]
  [--exact-runtime …] [--mode first_pass|all] [--verify-browser --accept …]`
  submits a SatisfyRequest and prints the settled request.
- `ato runtime-network serve --api … --token-file …` makes this machine a
  Runtime: advertise, report availability every 15 s, claim, execute, report.

## Out of scope (Phase 1)

Jev normalization and decomposition of acceptance prompts; LLM-generated D;
GitHub discovery; provisioning a Runtime; learned ranking; multi-service
routes; bindings (every binding is `binding_unavailable`); OCI routes on the
worker (`runtime.oci = false`); ato-managed Runtimes (refused unless
`allow_managed`, and none are enrolled).

## Security follow-up (blocker before the 100-app benchmark)

The Browser Verifier helper and its Chrome run as the worker's user with the
host filesystem visible. The origin boundary (ADR-020) constrains the network,
and the workload itself is contained (bwrap + landlock), but the verifier is
not. Before the 100-app benchmark, the helper and Chrome must run inside an
OS-level containment (bwrap: read-only system, private `/tmp`, only the
scratch/profile directory writable, no home directory) on every Runtime that
advertises `runtime.browser = true`. Until then, advertise the browser
verifier only on hosts where the worker's user holds nothing of value.

## Known limitations

- macOS has no containment, so it never admits a route with build steps or a
  process (`runtime.process = false`); only static routes run there.
- Facts are self-reported by an authenticated owner's Runtime. They are
  measured, not attested.
- Source is carried inline (≤ 32 MiB) and kept in R2 under
  `runtime-network/sources/<sha256>.tar`; there is no retention policy yet.
- Coordinator polling (3 s) and worker polling are fixed intervals.
