# Runtime Network 3c-b acceptance — 2026-09-25

Status: implemented and locally/Linux validated; not merged or deployed.
PRs: [ato #1403](https://github.com/ato-run/ato/pull/1403) and
[ato-api #691](https://github.com/ato-run/ato-api/pull/691).
No manual CI rerun, workflow edit, remote migration or environment flag change.
This evidence does not establish saved-object replay into a normal Hosted Run
(3d). Hosted lifecycle integration is recorded separately.

## Tree and isolation

Rust implementation: `87a6cd32a9daa702f1052a817d8ec9d9e76c71c7`, stacked on
Hosted ownership `e5be672ca0db5b6c1f3be9e6ba6f823c4506777a` (#1401), itself
on #1399 `ed1515bb050a798159588b4412e33517b9365151`.
API implementation after rebase: `d3ccba4a2a7f747b592b2d309e4fc43b663d8513`,
base `94782f5e6440ab1a261380cd6bf38a55bb6f5ca6`. Main advanced during work;
its changes did not add a migration. 0298 was still free. 0296/0297 are unchanged.

Linux ARM64: `oci-linux-test`, Rust 1.96, bwrap. Dedicated paths under
`~/ato-run/.tmp/formation-integrated-20260925/3c-branch/{src,target,scratch}`
and `3c-api/{state,worker-final-bundle}`. No target sharing with 2e base/branch.
The API used real routes, auth, D1, R2 and the existing compiled Rust receipt
Wasm in Miniflare 4.20250906.0, Node 20.20.2, loopback port 19440. The esbuild
harness supplies compiled Wasm modules and node:crypto compatibility; it does
not mock business services, auth, execution, receipts or storage. Earlier
harness module startup failures were corrected before recording acceptance.
A separate macOS wrangler-local Coordinator used port 19439 and separate D1/R2.
Tokens were newly generated for this isolated local DB, not production secrets.

## Actual source bytes and peak memory

Each static fixture has a capsule manifest, a small index and 16/64/128 files
of one MiB each from `os.urandom`. Raw tar is uploaded, downloaded, hashed,
measured and expanded. These are actual bytes, not size metadata, sparse files
or highly compressed padding. Every row below reached an attempt, HTTP K PASS,
the common receipt authority, one VerifiedRoute and Requester acceptance.
The final API includes server upload timeout. The latest rebased API main was
also tested with a new 64 MiB source (`rebased-api-64`).

| Payload | Verified archive bytes | Requester peak RSS KiB | Runtime peak RSS KiB | API/workerd peak RSS KiB |
|---|---:|---:|---:|---:|
| 16 MiB | 16,789,504 | 15,944 | 17,840 | 177,812 |
| 64 MiB | 67,145,728 | 15,816 | 18,324 | 171,544 |
| 128 MiB | 134,287,360 | 15,944 | 17,972 | 198,420 |

Requester/Runtime values are GNU `/usr/bin/time -v` from fresh processes.
For API values, restart its dedicated process per size and read Linux
`/proc/<workerd-pid>/status` VmHWM. Its initial VmHWM was respectively
111,164 / 111,852 / 111,744 KiB. This native process includes the local R2/D1
emulator and loaded modules; it is not a Cloudflare isolate heap measurement.
The additional 112 MiB of source does not produce an archive-sized copy in
Requester or Runtime; API peak rose about 20 MiB from 16 to 128 MiB and was
lower at 64 than at 16. Code inspection confirms fixed buffers/file passes in
all source stages. This does not claim zero allocation or no per-file metadata.

Cloudflare ingress limits are separate: Free/Pro requests are limited to
100 MB; the 128 MiB row is local acceptance, not deployed ingress acceptance.
64 MiB demonstrates the required >32 MiB path below that limit. The 256 MiB
object cap does not authorize a 10 GiB HTTP request. See ADR-032 for limits.

A separate single-file check placed one random 64/128 MiB `payload.bin` in
the source and served only `public/`. Both reached accepted VerifiedRoutes.
This separates source measurement/extraction from the unchanged artifact
materializer, whose per-file/process packing behavior is outside 3c-b.

| Single file | Archive bytes | Requester peak KiB | Runtime peak KiB | Stored artifact bytes |
|---|---:|---:|---:|---:|
| 64 MiB | 67,113,984 | 15,832 | 16,752 | 50,509 |
| 128 MiB | 134,222,848 | 15,716 | 16,480 | 50,509 |

## Fixed P0 repository

Node-RED `node-red/node-red@1e85f1efbc8875ad960400905038154a4e4dd76e`, the
existing P0 record's exact SHA and route, without source patches. Git metadata
is excluded by the existing snapshot rule; no LFS or submodules were added.

- Actual archive: **36,916,736 bytes**; measured tree: **35,383,564 bytes**.
- Satisfy: `01M3BCFVZFVRWH8VXCAMW97680`.
- Attempt: `01M3BCFW0CV1NATJ2S6ZEV0TNN`.
- Source ready -> assigned claim -> file download/hash -> tree measurement /
  extraction -> existing attempt -> dependency build.
- Known build failure: npm `getaddrinfo EAI_AGAIN registry.npmjs.org` while
  fetching npm-11.19.1.tgz. No source-limit refusal and no UNKNOWN downgrade.
- Transfer charged 36,916,736; expansion 35,383,564; stored 0. No K PASS or
  VerifiedRoute claimed for this failed build. No dependency/runtime fix added.

## Semantics and regressions

API: 80 targeted tests (source objects, Coordinator, byte-identical wire) on
latest main; typecheck, schema:check and schema:test-bootstrap pass. Migration
0298 only applied to dedicated local databases. Final server stream timeout
was separately tested against checksum, interruption and finalize races.

Rust: source 34/34; Runtime Network 29/29, including explicit old-memory vs
new-file closure/K/D equality, history-before-fetch, actual-byte caps,
publication failure, repeated delivery and physical interrupted worker cases.
The HTTP cap test also runs without Content-Length. gzip/zstd preserve v1/v2
closure; unsafe links, expanded limits, wrong size/digest fail before execution.

Actual Python and Node requests both produced accepted VerifiedRoutes. Their
first invocations used `network=denied` and correctly returned NotStarted;
explicit dependency-resolution policy then passed. These were known refusals,
not an UNKNOWN reset or automatic fallback search.

The 0297 ledger still freezes owner/search budget, consumes on claim, charges
same-attempt source redelivery once, charges another attempt again, and keeps
UNKNOWN reservations/consumption. Those cases, duplicate results, expiration,
foreign/unclaimed Runtime access and wrong/missing fence are covered by the
Coordinator suite. Physically restarting the local Coordinator between all
three memory trials preserved the previous search budget exactly.

Actual publication failure used stored budget 100 bytes for a 50,509-byte
artifact. Attempt `01M3BD040TSD91D064S5RYJRKQ` retained
`fully_satisfied = true` and runtime_verification=succeeded while publication
failed with `search_stored_budget_exceeded`; stored charge was 0 and no route
was accepted. JSON evidence includes the original receipt and attempt IDs.

## Remaining constraints

Ready grants and abandoned reservations retain bounded owner quota (16 total,
4 unfinished); no automatic GC/recycling service was introduced. A crash-left
uploading row fails closed. Recovery/quota lifecycle is a follow-up, not an
unbounded-storage workaround. Provider ingress and deployed CPU/memory limits
are unverified because deployment is out of scope.

Rollout after review: API migration 0298 -> API -> Runtime -> Requester.
Golden object-reference fixture is identical in both repos; legacy inline
normalizes through ready objects. Old Runtime clients require upgrade before
new object-reference requests (fence header/file acquisition). No rollout was
performed. Raw logs and harness scripts remain in the dedicated roots above;
`runtime-network-3c-b-2026-09-25.json` records the relevant IDs and evidence.
