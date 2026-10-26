# Runtime Network 3c-a search budget — local acceptance, 2026-09-25

No deployment, remote migration, feature-flag change or manual CI rerun was
performed. The Coordinator was ato-api `feat/runtime-network-search-budget`
under `wrangler dev --local`, with a fresh isolated D1 through migration 0296
and local R2. The requester and Runtime were the rebuilt Rust `ato` binary from
`feat/runtime-network-search-budget` on macOS ARM64.

## Cross-request cumulative budget

One search used these limits:

- `max_attempts = 2`
- deadline 3,600 seconds
- transfer, expanded and stored: 1 MiB each

Request R1 (`01M3B1PAZAJTBP14YECBABNFDY`) ran a static route whose required
`GET /missing` returned 404. Attempt `01M3B1PAZR9M9FZ2D1MVJ6WDXG` was a known
failure and consumed one attempt, 4,096 transfer bytes and 623 expanded bytes.

Request R2 (`01M3B1PZSGZZH2FVF06GCYEA77`) used the same search id and exact
budget. Attempt `01M3B1PZSRB7VT4JJQMBE5WH11` passed on the same actual Runtime.
The common Rust receipt authority accepted `fully_satisfied = true`, the
Coordinator persisted one VerifiedRoute, and the requester independently
accepted it.

The final durable search counters were:

| Resource | Used | Reserved | Remaining |
|---|---:|---:|---:|
| attempts | 2 | 0 | 0 |
| transfer bytes | 8,192 | 0 | 1,040,384 |
| expanded bytes | 1,271 | 0 | 1,047,305 |
| stored bytes | 50,509 | 0 | 998,067 |

This establishes that a later satisfy request neither resets nor replaces the
search budget.

## Stored-budget publication failure

A separate search (`search_3ca_stored_failure`) used a 100-byte stored limit.
Its actual static artifact was 50,509 bytes.

- satisfy: `01M3B1R6VK54VMTHN5V7F26PRV`
- attempt: `01M3B1R6WDDGXJM5N29JS3G7DM`
- runtime verification: succeeded
- Contract receipt: `fully_satisfied = true`
- publication: `failed(search_stored_budget_exceeded)`
- materialization and VerifiedRoute: absent
- stored bytes charged: 0

The receipt evidence remained in the attempt. Publication failure did not
rewrite K verification and a receipt PASS did not imply publication success.

## Automated verification

- ato-api Runtime Network Coordinator and wire fixtures, including 3c-a A–N
- ato-formation-worker native tests, including expanded and stored hard limits
- Rust/TypeScript byte-identical wire fixtures and search ceilings
- targeted Rust clippy with warnings denied, Rust fmt and API typecheck

The 32 MiB inline source transport remains unchanged. Content-addressed source
objects, streaming and cleanup remain for 3c-b or later.
