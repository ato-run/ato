# ADR-031 — Runtime Network budgets belong to the search

Status: proposed; stage 3c-a. Builds on ADR-021, ADR-028 and ADR-029.
Source transport (3c-b) is out of scope: the source still travels inline.

## Decision

Before this stage, `SatisfyRequest.budget.max_attempts` bounded one satisfy
request. A search could reset it by sending another request with the same
`search_id`. A budget now bounds the whole Formation search. It accumulates
across every request of the search and is never restored.

- **Authority.** The Coordinator owns the budget. It keeps the budget in a
  durable row keyed by `(owner_user_id, search_id)`. A requester states the
  budget once. A Runtime never reports what remains. It receives an
  attempt-local cap and treats that cap as a hard limit. Neither side can
  raise a limit by claiming budget is left.
- **Isolation.** Two accounts that use the same `search_id` string have two
  budgets. The id is opaque (ADR-028).
- **Freeze.** The first accepted request of a search creates the row, in the
  same D1 batch as the request row. Every later request must state an
  identical budget, including `mode`. Otherwise it is refused with
  `409 search_budget_mismatch`. A lower value is refused too: in v0 a budget
  never changes after creation, in either direction.

### Budget fields

`SatisfyRequest.budget`, internal wire `ato.runtime-network/0`:

| Field | Meaning | Hard ceiling |
|---|---|---|
| `max_attempts` | Attempts the search may claim | 32 |
| `mode` | `first_pass` or `all` | — |
| `deadline_seconds` | Search lifetime from its first request | 604 800 (7 days) |
| `max_transfer_bytes` | See *Byte units* | 10 GiB |
| `max_expanded_bytes` | See *Byte units* | 10 GiB |
| `max_stored_bytes` | See *Byte units* | 10 GiB |

A requester may ask for any value up to a ceiling. A value above a ceiling is
refused (`400 invalid_request`). Each 10 GiB ceiling is a search-wide
cumulative limit. It is **not** a single HTTP request limit, a single artifact
limit, or the capacity of any Runtime's disk. Free disk space on a Runtime is a
separate admission condition, and a Runtime that lacks space reports a
capability shortfall.

`mode` is a search objective, not a resource. Before this stage it was read
per request. It is now frozen with the rest of the budget: a search that began
as `first_pass` cannot continue as `all`. A different objective needs a new
`search_id`.

### Byte units

The byte budgets are **resource safety budgets**, not billing. Each counts
deterministic logical bytes, so the same search charges the same amount on any
Runtime.

- **transfer_bytes.** The logical bytes of content that the Coordinator
  authorizes a Runtime to fetch for an attempt. In 3c-a that content is one
  object: the decoded length of the inline source archive. HTTP framing,
  headers and the base64 of the submission are not counted. The unit of charge
  is one *logical object authorization*: `(attempt, source digest)` is charged
  once, when the attempt is claimed. A Runtime that fetches the same source
  twice for one attempt is not charged twice, and one that never fetches it is
  still charged. Another attempt of the same source is a new authorization and
  is charged again. When 3c-b adds content-addressed objects, the unit becomes
  `(attempt_id, content_ref)`. The ledger stays the same; only the set of
  objects in a ticket grows.
- **expanded_bytes.** The logical output bytes that result from materializing
  an archive or object on the Runtime. For the 3c-a source tree, these are the
  content bytes of its regular files. Directories and symlinks count as zero.
  Decompression buffers are not counted, and neither are build outputs,
  toolchain or dependency caches, or copies of the workspace. The build sandbox
  limits and disk admission govern those.
- **stored_bytes.** The logical bytes that the search authorizes a Runtime to
  retain as an artifact. For a process route, this is the length of the packed
  tar. For a static route, it is the regular-file bytes of the bundle
  directory. These bytes are charged whether or not the Runtime already held
  identical bytes, because the budget has to stay deterministic.

## Reservation and settlement

An attempt reserves its caps when its ticket is issued, so no Runtime begins
work that the budget cannot cover. Each row of the table below changes the
aggregate columns in the same SQLite statement as the attempt's status
compare-and-swap. This is done by triggers. Each transition happens at most
once, so reservation and settlement are idempotent under retries, duplicate
results and concurrent `advance()` calls.

| Attempt event | attempts | transfer | expanded | stored |
|---|---|---|---|---|
| ticket issued (`pending`) | reserved +1 | reserved + source | reserved + cap | reserved + cap |
| claimed | reserved → used | reserved → used | held | held |
| `pending` expires (not claimed, Runtime offline, deadline) | released | released | released | released |
| known result (`pass`/`fail`/`inconclusive`) | stays used | stays used | used + min(reported, cap); rest released | same as expanded |
| UNKNOWN | stays used | stays used | used + full cap | used + full cap |
| late result, owner resolution, `terminate_search`, new request | no change | no change | no change | no change |

**Attempt-local caps**, fixed at issue and never rewritten:

- **transfer** is the source length.
- **expanded** is `min(remaining expanded, 512 MiB)`. The 512 MiB is the
  source-expansion ceiling that every Runtime already enforces
  (`SourceLimits::default()`), so a larger cap could never be used.
- **stored** is `min(remaining stored, 512 MiB)`. The 512 MiB is the ceiling
  of one capsule bundle (`lib/objects`). An UNKNOWN attempt is charged its
  full caps, so an attempt's share must be bounded. If it were everything the
  search had left, one UNKNOWN would spend the whole stored budget, and a
  search the owner resolved could never keep another artifact. This is a new
  limit on a single Runtime Network artifact, separate from the 10 GiB
  search-wide limit, as the stage-3c plan anticipates: a per-object limit and
  a search budget are different numbers.

The ticket carries these caps as `resource_budget { transfer_bytes,
expanded_bytes, stored_bytes }`.

**Issue** requires all of the following:

- the current time is before `deadline_at`
- at least one attempt remains
- the remaining transfer budget covers the source length
- some expanded budget remains

If any of these fails, no ticket is issued. The request then settles
`exhausted` if candidates remain; otherwise the earlier rules apply
(`satisfied` or `unsatisfied`). If a new request can never receive a ticket
under these rules, it is refused at creation with `409 search_budget_exhausted`.

**One attempt in flight.** A search has at most one active request
(`idx_satisfy_requests_active_search`). That request has at most one attempt
in flight (`idx_satisfy_attempts_active`). A search therefore has at most one
`pending` or `claimed` attempt. Reservations are still atomic at the database:

- a `BEFORE INSERT` trigger drops a ticket that the remaining budget cannot
  cover
- `CHECK (used + reserved <= max)` holds on every aggregate
- the per-attempt reservation columns are immutable
- the charged columns are written once, in the claimed → settled transition,
  and a trigger refuses that transition without them

**Crash consistency.** Reservation, charge and release each happen in the same
statement as the transition that causes them. A Coordinator crash therefore
cannot leave a reservation without its attempt, or a settled attempt without
its charge. Recovery reruns `advance()`, which never reserves beside an
attempt already in flight.

**Refund.** No refund exists. After an attempt is claimed, nothing gives back
its attempt or its transfer: not failure, UNKNOWN, owner resolution,
`terminate_search`, a new request or the deadline.

## Runtime enforcement

The Coordinator's accounting is not enough on its own. The Runtime uses the
ticket's caps as hard limits.

- **Transfer.** The Runtime reads at most `transfer_bytes` of the source. A
  longer source is refused as `search_transfer_budget_exceeded`.
- **Expanded.** `SourceLimits.max_total_bytes` becomes `min(own ceiling,
  expanded_bytes)`. The limit applies while the archive is measured, before
  any file is written, and again during expansion. If the tree is larger, the
  attempt is refused before execution: `search_expanded_budget_exceeded`,
  `attempt_record: not_started`, and 0 bytes expanded. Once measurement
  passes, the tree's logical bytes are reported as expanded, even if
  materialization fails partway.
- **Stored.** The artifact is packed or measured before anything is written to
  the store. If it is larger than `stored_bytes`, it is not stored. The four
  ADR-026 outcomes are kept apart:
  - `runtime_verification` and the verification receipts stay as they were.
  - `publication` is `failed(search_stored_budget_exceeded)`.
  - The attempt fails in stage `publish` and reports `fail` with no
    `materialization_ref`.

  No VerifiedRoute follows, because a Runtime Network pass requires a
  published artifact. The receipts stay in the attempt's evidence.

The result reports `resource_usage { expanded_bytes, stored_bytes }`. The
Coordinator charges `min(reported, cap)`. A pass that reports usage above its
ticket's cap came from a Runtime that ignored its limits, so it is refused
(`inconclusive`) and never becomes a route.

**Settlement after a budget refusal.** A transfer or expanded refusal ends the
request as `exhausted`: every other candidate of the request would get the
same source and the same caps. A stored refusal is a known failure of that one
Derivation. The usual fallback rules apply, because another Derivation may
produce a smaller artifact. The stored budget is not charged for bytes that
were never stored.

## Deadline

The Coordinator sets `deadline_at` to the first request's `created_at` plus
`deadline_seconds`. No later request can extend it. After the deadline:

- **New request.** Refused with `409 search_deadline_exceeded`.
- **Issue.** No ticket is issued. A trigger compares the ticket's `created_at`
  with `deadline_at`. The request settles as described under *Issue*.
- **Claim.** A pending ticket can no longer be claimed; a trigger enforces
  this. The ticket expires and its reservation is released.
- **Claimed attempt.** Reaching the deadline alone does not rewrite a claimed
  attempt. The attempt ends with its result, or becomes UNKNOWN at
  `ATTEMPT_TIMEOUT_MS` as before. A result that arrives after the deadline
  follows the existing fence and UNKNOWN rules, so a valid pass still records
  its route.

This stage adds no semantics for terminating a running attempt.

## UNKNOWN (ADR-028)

UNKNOWN is never refunded:

- The attempt stays used.
- The transfer charged at claim stays charged.
- Expanded and stored are charged at their full caps, because what the attempt
  did is not known.

A late result, the owner's resolution, and `terminate_search` all leave the
budget unchanged. While an attempt of the search is unresolved, no ticket is
issued, so no reservation is made.

Because each cap is bounded at 512 MiB, an UNKNOWN attempt spends at most
512 MiB of expanded and 512 MiB of stored budget, never the whole search. A
search that continues after resolution keeps the rest. Finer per-attempt
shares are a stage-4 SearchState decision.

## Receipt authority (ADR-029)

Budget handling never produces or implies a PASS:

- Exhausting a budget does not turn a result into a receipt PASS.
- A publication budget failure keeps its receipts but records no route.
- A receipt PASS is not publication success.

A VerifiedRoute still requires the common Rust receipt authority through
`passRefusal`. No budget code path skips it.

## Persistence

ato-api migration `0297_runtime_network_search_budget.sql` makes these
changes:

- It adds `runtime_network_searches`:
  - the frozen limits
  - `deadline_at`
  - `used` and `reserved` for attempts, transfer, expanded and stored bytes
  - `budget_version = 'ato.runtime-network.search-budget/0'`
- It adds per-attempt `reserved_*` and `charged_*` columns to
  `satisfy_attempts`.
- It recreates the `satisfy_requests` creation trigger, so that
  stop → UNKNOWN → budget mismatch → deadline is checked in one fixed order.

It does not add full SearchState, candidates or evidence.

Searches created before the migration are backfilled with `budget_version =
'legacy'`:

- Limits are at least what they already used.
- Byte usage is 0, because it was never measured.
- No ticket is issued for a legacy search, and no request can join one.
- A pre-budget pending ticket has zero caps, so a Runtime refuses it.

## Wire and deployment

This is a coordinated change to the internal `ato.runtime-network/0`
protocol:

- `SatisfyRequest.budget` gains fields.
- The ticket gains `resource_budget`.
- The result gains `resource_usage`.

Both repositories hold byte-identical fixtures: the satisfy request, the
ticket, the attempt results and the budget ceilings.

A mixed deployment is not supported:

- An older Runtime ignores `resource_budget` and runs uncapped. Its result has
  no `resource_usage`, is refused, and ends UNKNOWN.
- A newer Runtime cannot read a ticket without `resource_budget`.

Merge both PRs together. Deploy the ato-api migration and Worker together with
the Runtimes.

## Out of scope

- sources larger than 32 MiB
- streaming or content-addressed source transport (3c-b)
- Go and Java
- full SearchState
- AI, token and monetary cost budgets
- Hosted Formation budgets
- a scheduler or ranking change
- cleanup of old searches
