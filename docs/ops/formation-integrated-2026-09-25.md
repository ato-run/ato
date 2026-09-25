# Formation sequential integration — 2026-09-25

One sequential track inherited existing verification, completed 2e-b, then
3c-b, then integrated them. No second parallel track/task was created.
Implementation and targeted Linux integration are complete; the full Chrome
E2E remains a baseline-reproduced failure. All new PRs are unmerged. No
production/staging deployment, remote migration, flag change, manual CI rerun
or workflow change was performed. Every new commit has `[skip ci]`.

## PRs, exact review bases and dependencies

| PR | Base branch / SHA | Head branch / SHA |
|---|---|---|
| [ato #1399](https://github.com/ato-run/ato/pull/1399), existing common Hosted verification | main / `aaaaaa15d0cfafdf397977bcabb8eb189fb5c2c2` | refactor/hosted-validator-common-verification / `ed1515bb050a798159588b4412e33517b9365151` |
| [ato #1401](https://github.com/ato-run/ato/pull/1401), 2e-b | refactor/hosted-validator-common-verification / `ed1515bb050a798159588b4412e33517b9365151` | refactor/hosted-process-ownership / `e5be672ca0db5b6c1f3be9e6ba6f823c4506777a` |
| [ato-api #691](https://github.com/ato-run/ato-api/pull/691), 3c-b API | main / `94782f5e6440ab1a261380cd6bf38a55bb6f5ca6` | feat/runtime-network-source-objects / `2bf5d54288715bb7f118aed7fee12b24e932496d` |
| [ato #1403](https://github.com/ato-run/ato/pull/1403), 3c-b Runtime/Requester | refactor/hosted-process-ownership / `e5be672ca0db5b6c1f3be9e6ba6f823c4506777a` | feat/runtime-network-source-objects / `649cb2051811e7526d6cda8cccf3048d7058a791` |
| This documentation-only integration PR | feat/runtime-network-source-objects / `649cb2051811e7526d6cda8cccf3048d7058a791` | docs/formation-integrated-verification; exact final head is shown on its PR |

Recommended merge order: #1399 -> #1401 -> API #691 -> ato #1403 -> this
integration record. The API/Rust pair should be reviewed together. Existing
#1399's normal merge was rejected by branch protection; no admin bypass was
used. The stack allowed independent downstream work to finish without merging.
After a squash merge, maintain the intended stacked diff when retargeting.
Future deployment order is separately 0298 -> API -> Runtime -> Requester;
merge authorization does not authorize deployment.

## Hosted ownership boundary

The existing `LaunchedProcess` already launched Hosted LocalProcess. No new
wrapper, fake K, fake AttemptSpec or fake journal was introduced. Common
ownership now covers group-aware exit confirmation, bounded TERM/KILL stop
and Drop cleanup. Group liveness fails closed. Unused session start/finish
logic was removed; Hosted lease start/finish is the sole production path.
Failed readiness retains process identity for existing quarantine/recovery.

argv/cwd/env/toolchain selection, workspace/state mounts, bindings/network
permissions, readiness, observe_url and receipt schema/identities are preserved.
Hosted owns lease/fence/slot, authorization/renewal, owner stop, state writer,
restore/commit/release/quarantine and route publication. Normal order remains
stop -> confirmed disappearance -> pack/commit -> writer/lease release.
Unconfirmed stop does not commit or release the writer. The common handle
never commits/releases state. Static Hosted remains static; OCI/service groups
remain 2e-c. Snapshot expectation and actually restored evidence stay distinct.

See [2e-b comparison](formation-2e-b-ownership-2026-09-25.md).

## Source transport boundary

See [3c-b evidence](runtime-network-3c-b-2026-09-25.md), its
[receipt/attempt JSON](runtime-network-3c-b-2026-09-25.json), and
[ADR-032](../rfcs/draft/ADR-032-runtime-network-source-objects.md).
Archive byte SHA-256 is transport identity; resolver tree closure is source
identity. Ready requires actual stored size/hash verification. Owner grant,
assigned Runtime, claim, expiry and fence authorize source access, not digest
knowledge. Existing STORE_BUCKET, source relay and 0297 budget are reused.

Real 16/64/128 MiB random source, and separate single-file 64/128 MiB source,
passed Requester -> source object -> Runtime file acquisition -> resolver ->
attempt -> Rust receipt authority -> accepted VerifiedRoute. Single-file
Requester/Runtime peak RSS stayed about 15–16 MiB. Fresh local API workerd
peaks were 177,812 / 171,544 / 198,420 KiB for 16/64/128 MiB multi-file source;
that includes native D1/R2 emulation, not just the Worker isolate heap.
No archive-sized memory copy was introduced in source transport.

Fixed P0 Node-RED SHA `1e85f1efbc8875ad960400905038154a4e4dd76e` transferred
36,916,736 bytes, expanded 35,383,564 and reached dependency build. npm DNS
EAI_AGAIN is the next observed blocker; no build/runtime workaround was added.
Cloudflare plan ingress can be below the 256 MiB object limit; 128 MiB is local
acceptance. No claim of deployed ingress/CPU/memory acceptance is made.

## Initial integrated checks (historical)

All Linux checks used the same source tree containing 2e-b and 3c-b, under
`~/ato-run/.tmp/formation-integrated-20260925/3c-branch/`. Dedicated src/target/
scratch, Coordinator D1/R2 and token were separate from the baseline and
other tasks. Linux test profile debug=0, Rust 1.96, fd limit 65,536.

| Boundary | Result |
|---|---|
| Common runtime | 54/54: group cleanup, ownership, verification, journal |
| Hosted runtime_launch | 83/83: actual stateless/stateful SQLite lifecycle, stop/restore/commit, auth/owner-stop, quarantine/recovery |
| #1399 Hosted verification | 11/11 with actual contained LocalProcess shim; readiness PASS cannot overwrite K FAIL |
| Secret binding/process launch | 1/1: workload-only binding proof, no value in evidence, group removed |
| Source resolver | 34/34: v1/v2, gzip/zstd, digest/size, expansion/link safety |
| Runtime Network | 29/29: old/new closure/K/D equality, source/history ordering, UNKNOWN, publication and interrupted worker |
| Actual Runtime Network | Static 16/64/128 MiB; Python; Node; latest-main API 64 MiB; single-file 64/128 MiB all accepted |
| API | 80 source/Coordinator/wire cases; typecheck; schema:check; schema:test-bootstrap |
| Hygiene | fmt; targeted clippy with warnings denied; arch-check 43 packages; identical wire bytes |

A first integrated Hosted run had 80 pass/3 fail because its configured real
worker shim had not been built (`--bin ato` selected only the CLI). Explicit
worker build corrected the harness; all 83 then passed. No code/test assertion
was weakened. API main advanced to 94782f5e: rebase, all 80 regressions and a
fresh actual 64 MiB acceptance passed afterward. The HTTP cap test was also
extended and passed locally without Content-Length; its production code did
not change after the Linux checks.

Known full-suite exception: installed ARM Chrome aborts with SIGABRT before
CDP in the Hosted browser E2E. The identical case fails on baseline ed1515bb
and the 2e-b tree. It was neither skipped nor described as passed. Actual
HTTP/static/process validation above is not a claim of full Chrome acceptance.

## Budget, UNKNOWN and publication

Actual Coordinator restarts preserved prior search counters exactly. Tests
cover same-attempt re-download once, another attempt charged again, frozen
owner/search/mode/deadline, atomic reservations, claim consumption, UNKNOWN
non-refund, duplicate result and source failure after prior Started/UNKNOWN.
No automatic new search id was used to resolve uncertainty.

Actual artifact storage limit (100 bytes vs 50,509-byte artifact) produced
publication failure with a retained passing K receipt, stored charge zero,
and no VerifiedRoute. Source upload/storage quota is separate from logical
Runtime transfer. Pending/expired uploads remain bounded and occupy quota;
crash-left uploading rows fail closed. Automatic quota recycling/GC is a
follow-up, not an unlimited-object loophole.

## Cleanup and remaining work

[Cleanup audit](formation-integrated-cleanup-2026-09-25.json): no owned
workload/Chrome/Coordinator processes remained; all baseline/branch scratch
and all Runtime/Requester work directories were empty. Dedicated local
Coordinator and SSH tunnel were stopped; newly generated token files were
removed. Evidence, local D1/R2, source fixtures, artifacts and independent
build targets are retained for inspection. No other task's files/processes,
shared toolchain or existing credentials were removed.

Implementation: 2e-b/3c-b complete for review. Integration: targeted checks
above complete; full Chrome gate failed on baseline too. Merge: new PRs open,
#1399 awaits normal review/policy requirements. Deployment: none by this track.
Next work stays numbered: 2e-c OCI/service groups, 2f IR reduction, 3d retained
object replay, 4 SearchState. No full SearchState, LLM/Jev D generation, new
runtime families/services/bindings, mass app run or deployed feature enablement
is claimed.

## Final 3c-b archive hardening review

The source-only follow-up is recorded separately in
[hardening acceptance](runtime-network-3c-b-hardening-2026-09-25.md) and its
[JSON evidence](runtime-network-3c-b-hardening-2026-09-25.json). At baseline
`2a5d0bf9`, a PAX size of 65536 with ordinary size 0 bypassed the 1 KiB cap:
64 KiB was materialized while usage reported zero. Adding a large GNU longname
after that file also bypassed the raw metadata scan, reaching the cooked path
check. Both failures were reproduced before changing code, with locked tar
0.4.46 and correct archive digests.

Fix `9fad7cdc` refuses PAX size boundary overrides and GNU sparse in bounded
preflight before cooked metadata allocation. Ordinary bounded PAX is retained.
Memory/file measurement and expansion share this check, then enforce effective
entry size and actual read/write byte limits separately from framing allowance.
Normal v1/v2 identity remains unchanged; no receipt schema or K/D rewrite.

Final Rust review head: `649cb2051811e7526d6cda8cccf3048d7058a791`.
API review head: `2bf5d54288715bb7f118aed7fee12b24e932496d` (documentation-only
hardening diff, executable code unchanged). This integration branch was rebased
onto that Rust head; its diff contains only the three integration/roadmap files,
not duplicated parent implementation.

- Formation: 183 tests pass, including 41 source cases. Runtime Network: 30
  pass; the manual request fixture export was executed separately. fmt,
  targeted clippy and arch-check (43 packages) pass; wire bytes are identical.
- Real API upload/finalize makes both hostile archives ready after size/SHA-256
  verification; actual Runtime acquisition refuses them before materialization
  or build. No marker/VerifiedRoute/tree remains; expanded usage and actual
  expansion are both zero. Started/UNKNOWN histories remain conservative.
- Separate 64/128 MiB raw single-file source reruns reach accepted VerifiedRoutes.
  Their closure/K/D exactly match the prior records. Runtime peaks are
  16,472/16,608 KiB, Requester peaks 15,524/15,564 KiB; actual source bytes match
  charged expansion (67,109,528 / 134,218,392). No archive-sized copy appears.
- Fresh local Coordinator/token were cleaned up, all work/scratch directories
  are empty. The new cleanup audit is in the hardening JSON; the earlier
  cleanup record above remains historical.

Hosted/Chrome/Python/Node/P0 and API unit checks in the initial table were not
rerun for this source-only follow-up. The existing Chrome baseline failure and
P0 npm DNS blocker remain unresolved; no 3d replay claim is added. Stages retain
their numbers. Implementation and targeted integration are complete for review;
merges/deployment are still pending. #1399 remains protection-blocked, with no
bypass. Merge order remains #1399 -> #1401 -> API #691 -> ato #1403 -> #1404.
