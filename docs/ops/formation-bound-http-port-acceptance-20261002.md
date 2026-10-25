# Source-backed private HTTP Port acceptance

Source-owned setup may require an HTTP interaction after launch and before the
unchanged frozen K can pass. The typed execution plan names an inspected Source
basis, logical Port, GET/POST template, protected input references, accepted
status codes and optional GET/status guard. The existing HTTP Adapter owns wire
encoding; the common Source/retained Runtime owns authority, current grant
resolution, original execution deadline and confirmed/uncertain stop handling.
Returned bodies, headers, tokens, input values and status-line reason text are
discarded. An uncertain POST response is never automatically replayed.

The configured capability requires containment, a scoped execution gate and a
current protected input-grant path. The standalone probe and unbound routes do
not claim it. Static Web, OCI and portable routes currently reject these
operations. No app command preset, Source rewrite or K change was introduced.

## Fixed code and evidence pins

| Component | Pin |
| --- | --- |
| Runtime/CLI/Linux binaries | `601f71ea3ce47c1750f2fae5c97a10121c512df4` |
| Measurement harness | `bbc1d62b91707abbd0c16c7e385edb191c0c99df` |
| API receiver | `02a9e58bbf2f8b27c6f976107995c5c14f662a8f` |
| Rust WASM authority source | `0fd912f6754541a3cbfe33cd6f0f573343ca11b4` |
| WASM SHA-256 / bytes | `9420802a52f32a021fb0839ef64d46f63299a8b5db59ed92c9522c483217987b` / 2,282,978 |

`0fd912f6..601f71ea` repairs capability propagation from the advertisement into
the actual Source and retained attempt admission profile; canonical authority
code and authority lockfile are identical. `601f71ea..bbc1d62b` only corrects the
measurement parser for the dedicated CLI `needs_input` result. Rust execution
code and WASM authority are unchanged. Documentation commits do not advance an
execution pin.

## Actual Coordinator/Runtime controls

[Public control proof](evidence/formation-bound-http-controls-20261002.json)
SHA-256: `df3fa0d0fdb39fd40d61d15fd20263d9bf7b696c2a1fa5b52b6a9e33d899b935`.
These owned fixtures use preregistered fixed exchanges; they do not add an OSS
app success or an autonomous provider acceptance.

| Control | Actual outcome |
| --- | --- |
| Matching scope, guarded private POST | Fresh same-K PASS; result ACK and closed delivery; encrypted value revoked |
| POST accepted, response lost | `source_runtime_http_operation_failed`; no K receipt or automatic POST replay; result ACK and closed delivery |
| Input outside application scope | CLI `needs_input / scope_mismatch`; no candidate execution; original deadline ends the claimed Runtime attempt; result ACK and closed delivery |

All three scans found zero private-value occurrences in their public files and
zero encrypted values after owner cleanup. The initial capability-admission
failure is preserved. A separate scope-outside trial interrupted by the old
measurement parser is also preserved; it is not counted as a completed control.

[Interrupted trial's actual UNKNOWN proof](evidence/formation-bound-http-interrupted-unknown-20261002.json)
SHA-256: `fd99fbc71ec584fa8c4dc74ef014fbe2b45d44c943e92a4ccd4d14011f019c2b`.
Attempt `01M3XJH1NBDEZ64P48D0ND1BPK` was claimed at
`2026-10-02T05:48:52.922Z` and became UNKNOWN on the read at
`2026-10-02T06:19:36.576Z`, after the real unchanged 30-minute claim timeout.
Only the same persisted local Coordinator was reopened for reads. No Requester,
Runtime, inference, Source execution, saved-result submission, clock reset or
budget reset occurred. Input cleanup had already removed the encrypted value;
producer evidence hashes remained unchanged. Old WBO UNKNOWN was not rerun.

## Kutt Source acceptance

[Public Kutt proof](evidence/formation-kutt-private-http-20261002.json)
SHA-256: `a13564af2466aabf300c882b1809fb081a6bbec5329088290e9a81ed8d1d622f`.

Source: Kutt 3.2.6, commit `279b491b53bbd01fbae70f603222526962772061`;
immutable closure `sha256:dfaadb8c7df176e72ef6900df8d33b752c2afdafb4b2317752ec647e1a0e5145`.
Both trials start without a known D, inspect root declarations and explicit
references through the shared session path, and keep K
`sha256:2940ae2d3538206497a9cf7fcaf66521556d009b38524679f0643df63a7fc73d`.

The first declared plan built the native dependencies and ran Source-owned
`migrate`/`start`; the protected owner-creation POST returned 201. Root still
returned 302 under the Source default `DISALLOW_ANONYMOUS_LINKS=true`, producing
`http_status_mismatch`. That closed Search and its six consumed session exchanges
remain immutable. A separately preregistered repair Search uses the declared
`DISALLOW_ANONYMOUS_LINKS=false` setting; neither K nor Source was rewritten.

The repair Search `search_74240d7416d1e2acf1556586147afdc6` produced D
`sha256:1c5a59c45a569c3400fe4993e0068e27eed63cbddc6602353deea09f776def66`.
Attempt `01M3XKN9K0BGBVFMPZ8BP6Y0XE` returned a fresh receipt with root HTTP 200,
matching Source identity and `fully_satisfied=true`. The actual Coordinator
accepted it with no metadata mismatch. One execution, two rounds and seven
session exchanges completed; inspections did not consume additional rounds.
The original Search deadline remains `2026-10-02T06:37:47.130Z`. Delivery was
ACKed and closed; attempt/transfer/expanded/stored reservations are zero.

The generated plan uses production-only lockfile acquisition, the registered
native rebuild operation for `better-sqlite3` and `msgpackr-extract`, pinned
Python/GCC/Make, denied build/Runtime network, Source-owned migration/start and
explicit `app.data` state. Its owner interaction uses current encrypted-store
grants for an owned local verification account; JWT is temporary. The public
scan checked 1,136 files and found zero private owner-password occurrences. Both
reusable owner values were revoked and the encrypted store contains zero values
after final cleanup.

This is **repair PASS across two separately preregistered Searches**, not initial
app PASS, a restarted Search or functional/persistence acceptance. Successful D
remains `k_reached_awaiting_assessment`; ordinary Run permission, publication,
deployment and remote migration were not created. The local Source-owned app
migration is distinct from a remote API database migration.

## Validation and remaining gates

Local component suites passed: HTTP 6, Formation 122, proposal registry 34,
authoring 3, Runtime 95, Worker 79, portable 94 and authority ABI 7. All-target
clippy passed. Linux HTTP 6, Runtime 96 / one existing ignored, registry 34,
authoring 3 and Worker 80 passed; CLI/Worker/preflight builds passed. Architecture
checks passed for 43 packages on both systems. API typecheck passed; eight new
actual Rust/WASM tests and 201 existing focused tests passed. Two initial new
fixture failures were retained and corrected by supplying a required historical
nullable field; production validation was not relaxed.

Latest code CI is not fully green: Ubuntu/architecture/snapshot passed; macOS and
Windows failed. API activity and browser state-sync CI also failed. Only failures
with a verified same-condition base comparison may be described as existing;
the current process-worker exit and browser WebSocket assertions still require
that comparison. Component/local acceptance is not CI success.

Kutt feature and confirmed-stop/restart state retention remain pending, as do
final-pin SVGOMG/OCI/changedetection regressions and independent API app arms.
Codex has three unique OSS app typed-K successes across recorded pins (SVGOMG
repair, changedetection initial, Kutt repair); API has zero accepted unique apps.
Those counts are not final-pin completion or 100-app reachability.

No paid API calls were made for these controls or Kutt sessions. Actual Codex
provider call/token/cost data is not exposed and remains unknown. The historical
account ledger stays at 18 used call slots, $0.082825 estimated/unknown charge,
$0.482081 remaining and no unsettled provider reservations. The approved small
gate has used 1 of 24 slots, leaving 23. The earlier HTTP 401 is retained as
charged-unknown with a settled estimate; it is not free or retried. A valid key
for the same authorized verification account is still needed for independent
API acceptance. No call allocation for the later 100-app measurement was made.
