# ADR-021 — Runtime Network Phase 1: satisfying one K across owned Runtimes

**Status**: proposed
**Context**: Runtime Network Phase 1, on top of ADR-019 (ephemeral verification)
and ADR-020 (Browser Contract v0). Coordinator: `ato-api`
`src/services/runtime_network/`, migration `0288_runtime_network.sql`.

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
  what a Runtime *is*; `POST /runtimes/self/availability` stores whether it can
  take work *now* (`runtime_availability`: capacity, current slots, health,
  observed time). The worker reports availability every 15 s and immediately
  whenever it takes or frees a slot. A Runtime that stops reporting for 60 s is
  offline: it is not a candidate, but its identity, its facts and the routes
  verified on it stay.
- **Capability profiles are immutable evidence.** An advertised fact set is
  stored once under its content address (`runtime_capability_profiles`:
  `facts_ref` = SHA-256 of the JCS fact map, `facts_json`, `first_seen_at`;
  never updated). `runtime_environments` is only the current pointer
  (`current_facts_ref`, `agent_version`, `advertised_at`). A route's
  `capability_profile_ref` therefore keeps resolving to the exact facts it
  was verified under after the Runtime re-advertises
  (`GET /capability-profiles/:ref`, visible to the owner of a Runtime, route
  or attempt that refers to it). The worker implementation version is
  provenance, not a fact: the attempt records the version the environment
  advertised when the ticket was issued, and the route the version the
  executing worker attested.
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
  bind to the same Contract: one request is one K. A repeated
  `derivation_ref` is refused (it would only spend budget twice), candidates
  are unique per D × environment, and without a browser Contract
  `contract_ref` must equal `base_contract_ref`.
- **Request metadata is a hint, the route is the authority.** Effects,
  requirements and provisions in the request come from the requester, so
  they cannot be proof of the canonical Derivation. The coordinator uses them
  only to prune and order candidates. Everything safety depends on is
  re-established by the Runtime from the ticket's archive and route before
  anything runs (see Execution), and fallback is decided from what the Runtime
  attests.
- **Strict browser Contract.** `browser_contract` is validated as
  `BrowserContractV0` exactly as the Runtime deserializes it
  (`deny_unknown_fields`, `ato.browser-contract/0`, `browser.task` criteria,
  `v0.identity`, prompt ≤ 4096 bytes), so no ticket carries a Contract the
  Runtime cannot read. The effective-K recomputation (JCS) stays in Rust: the
  Runtime refuses a ticket whose effective ref its own planning does not
  reach.
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
- **Re-filtered at issue time.** Just before a ticket is issued, the whole
  hard filter runs again against the Runtime as it is then — runner status and
  drain, ownership and management policy, the exact constraint, availability,
  health and capacity, the current capability profile, verifier, bindings and
  effect policy. A candidate that no longer qualifies is not ticketed; its
  `filter_reasons_json` is replaced by the current reasons and the next
  candidate is considered.

### Execution

- **Fixed ticket.** `satisfy_attempts.ticket_json` is written once, with the
  D text, the Runtime, the environment, the archive digest and the browser
  Contract. Neither the coordinator nor the Runtime rewrites it.
- **The Runtime re-establishes the ticket before running it.** From the
  ticket's archive and route alone, the worker freezes I, plans the canonical
  Derivation and computes `derivation_ref`, the effective `contract_ref`, the
  effect class, requirements and provisions. It refuses — `inconclusive`,
  nothing of the candidate executed — when: the ticket names an environment
  it does not execute (`environment_mismatch`; Phase 1 executes only
  `native`); the ticket carries bindings (`bindings_unsupported`); its refs
  differ from the ticket's (`ticket_mismatch`); the canonical effect class is
  not disposable (`effect_policy`: a network attempt runs unattended and may
  be retried elsewhere). The local driver's admission then refuses, still
  before execution, a route whose `[[platform]]` excludes this host
  (`platform_unsupported` — the same rule `--runtime local` now applies),
  a missing containment for a build step or a process, a missing toolchain
  root, a denied network, and a browser Contract without a verifier
  (`browser_verifier_unavailable`).
- **Attestation.** Every result carries `attestation {environment_id,
  agent_version, derivation_ref, contract_ref, effects, requirements,
  provisions, execution_started}`. The coordinator compares it with the
  request's metadata and records `metadata_mismatch` (field, claimed,
  attested) on the attempt. A reported pass is accepted only when result and
  attestation name the ticket's D, K and environment, `execution_started` is
  true and the attested effects are disposable; otherwise it is recorded as
  `inconclusive` with a `coordinator_note`.
- **One ticket at a time, by database invariant.** `advance()` runs after
  every change and may run concurrently (a result, the requester's polling,
  another Runtime's claim). `UNIQUE (satisfy_id, candidate_id)` and a partial
  unique index allowing one `pending`/`claimed` attempt per request make a
  second issuance a no-op (`INSERT … ON CONFLICT DO NOTHING`), whichever
  caller gets there first.
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
  execution_environment_id, capability_profile_ref, agent_version,
  verifier_receipts[], verified_at}`, one per passing attempt.
  `capability_profile_ref` is the environment's profile at issue time
  (immutable, resolvable later); `agent_version` is the executing worker's,
  as attested. A VerifiedRoute is evidence that
  this D on this R once satisfied this K; it is not part of K's identity, and
  several routes for one K are expected.
- **INCONCLUSIVE never becomes a route.** Only an accepted `pass` is written
  to `verified_routes`.
- **Fallback is decided from the Runtime's attestation.** After a non-pass,
  never under an exact constraint. Otherwise: if a Runtime attested a
  non-disposable effect class, no fallback (every Runtime would refuse it; the
  request ends `unsatisfied`); if nothing of the candidate ran — the ticket
  was never claimed, or the Runtime refused before execution
  (`execution_started = false`) — the next candidate is tried, since it
  re-establishes the route itself before running; if it ran, only when the
  attested effect class is disposable (`pure`, `idempotent`,
  `record-substitutable`). A claimed attempt that never reported has no
  attestation, so it does not fall back. The request's `effects` never
  decides. Every attempt stays in the history.
- **Budget.** `first_pass` stops at the first route; `all` tries every
  admissible candidate within `max_attempts`.

### Surfaces

- `ato form <dir> --runtime local` is unchanged.
- `ato form <dir> --runtime-network --api … --token-file … [--route …]
  [--exact-runtime …] [--mode first_pass|all] [--verify-browser --accept …]`
  submits a SatisfyRequest and prints the settled request.
- `ato runtime-network serve --api … --token-file …` makes this machine a
  Runtime: advertise, report availability (every 15 s and on each slot
  change), claim, re-establish, execute, attest, report.
- `GET /v1/runtime-network/capability-profiles/:ref` resolves a profile.

## Out of scope (Phase 1)

Jev normalization and decomposition of acceptance prompts; LLM-generated D;
GitHub discovery; provisioning a Runtime; learned ranking; multi-service
routes; bindings (every binding is `binding_unavailable`); OCI routes on the
worker (`runtime.oci = false`); ato-managed Runtimes (refused unless
`allow_managed`, and none are enrolled).

## Security follow-up (resolved by ADR-022)

The Browser Verifier helper and its Chrome ran as the worker's user with the
host filesystem visible. ADR-022 contains both (bubblewrap, allowlisted
filesystem, the browser in its own PID namespace with no inherited
environment, model keys on a file descriptor). A Runtime advertises
`runtime.browser = true` together with `verifier.browser.containment = bwrap`
only when that sandbox starts, and the coordinator gives a browser Contract
to no other Runtime (`verifier_containment_unavailable`).

## Known limitations

- macOS has no containment, so it never admits a route with build steps or a
  process (`runtime.process = false`); only static routes run there.
- Facts and attestations are self-reported by an authenticated owner's
  Runtime: measured and recomputed by the worker, not hardware-attested. A
  Runtime is trusted to run its own owner's tickets; the rules above keep a
  requester's metadata from steering it, not a compromised worker.
- A route whose attested effects are unsafe ends the whole request, even when
  other authorized Ds remain.
- Source is carried inline (≤ 32 MiB) and kept in R2 under
  `runtime-network/sources/<sha256>.tar`; there is no retention policy yet.
- Coordinator polling (3 s) and worker polling are fixed intervals.
