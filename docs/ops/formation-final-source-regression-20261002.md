# Formation final Source and functional regression

The merged foundation remains fixed at Ato `8fca0f4b78b1bed92eae21823e5c5f7207058271`
and API `9e0031900836090612b94e775e26f980a68f32d7`. The final CLI, Worker and
preflight code pin is `3bb28192419f4a12cf2566cab57a8f641cf88018`; the local
Coordinator receiver is API `02a9e58bbf2f8b27c6f976107995c5c14f662a8f` with all
three required WASMs. Receipt authority remains `0fd912f6754541a3cbfe33cd6f0f573343ca11b4`
(SHA-256 `9420802a52f32a021fb0839ef64d46f63299a8b5db59ed92c9522c483217987b`).
The subsequent head `5f3bd350` changes the acceptance ACK observer only; it
reads completed attempt IDs from root-owned delivery records without publishing
tickets or weakening permissions. Earlier `c0eec529` contains evidence/docs only.
The SVGOMG, OCI, changedetection.io and input controls below retain these
execution pins. The later pre-dispatch budget correction uses Ato
`da7c452c1dcd6011399d2e12c7cb47e653e40ccd` / API
`9b483d4a7df9aa2370636bd1b60c2fbcda98320b`; it does not relabel those earlier
measurements. Ato `252d07a0` removes a redundant `Ok(...?)` only, after freezing
the new binaries. [Budget control evidence](evidence/formation-pre-dispatch-budget-control-20261003.json)
records the new execution pin and receipt-authority hash.

## v0 scope and remaining gates

| Area | Implemented / measured scope | Remaining or outside v0 |
| --- | --- | --- |
| Search control | Shared persisted round/budget/deadline state, custom retry transport, bounded phases and backoff, typed terminal distinctions; new pre-dispatch budget control and original-clock input controls | Actual provider usage/fees for the Codex session are unavailable; independent paid API acceptance awaits valid credential input |
| Routes | Static Web, one Node/Python process and one OCI service; typed common proposals; bound capability checks and unchanged-K Runtime receipt | Compose/multiple services, undeclared monorepo/multi-repo traversal and automatic source/Dockerfile edits are outside v0 |
| Dependencies | Source-owned Python requirements, isolated sdist wheel building, pinned wheel artifacts/offline installation; explicit npm lifecycle/native rebuild plans with bound toolchains and limits | No application-specific setup commands in presets; unrestricted build/runtime networking is not admitted |
| Private input | Encrypted store, scoped metadata/assignment/grants and dedicated input surfaces; measured matched/outside/expired/revoked/ambiguous inputs, temporary cleanup and embedding controls | Final-pin PWA input/permission-change resume is unmeasured; fixed nonsecret owner email in the measurement controller limits the orchestration privacy claim |
| Function/state | SVG UI; changedetection watch create/edit/pause across restart; Kutt native dependencies, migration, owner/API operations and committed state across stop/new Run | These retain separate functional pins/artifacts; automatic production Source state provisioning and external credential authentication remain unverified |
| Admission | Success submitted as assessment pending, with no conversion to normal Run permission | Deployment, feature flags, remote migrations, production enablement and 100-app rerun are separate, unperformed work |

The historical unique OSS count remains **Codex 3 / API 0**: SVGOMG,
changedetection.io and Kutt. The OCI representative is a controlled single
service fixture and adds no unique OSS app. SVGOMG and Kutt are repair PASS;
changedetection.io passed its first executing build after a preserved
preexecution infrastructure failure; OCI passed its first execution after a
same-round schema correction. Failed/interrupted Searches and UNKNOWN remain
separate records, rather than being removed from the denominator. New 100-app
in-scope success rate and overall reach are not claimed before remeasurement.

Code after the prior `3bb28192` measurement adds only the pre-dispatch budget
boundary and its strict API completion/migration/authority wiring. New Kutt and
budget controls ran at `da7c452c` / `9b483d4a`; later Ato `252d07a0` fixes a
redundant clippy expression, and subsequent Ato commits add evidence/docs only.
Earlier SVG/OCI/Python/input/function/state results keep their exact old pins.
Local/CI/fixture acceptance is not reported as staging or production validation.

## SVGOMG

[Source and separate functional evidence](evidence/formation-svg-source-functional-final-20261002.json)
(SHA-256 `3acbac1a5cd690d4af0a777a88629a26c514641f8790443ad92cfc459e5792f0`)
records a fresh zero-known-D Codex session Search using the original Source
and K `6b224432…`. Proposals were derived from this Search's verified root
manifest, lockfile and explicitly referenced gulpfile; no earlier success D
was supplied. The admitted attempts were: `authority_denied` before execution,
then measured `npm_lifecycle_plan_required` for locked `es5-ext`, then repair
PASS. The generic offline `npm_rebuild` operation selected that package using
the explicit bound Python/GCC/Make toolchains. An initial Node entry in its
auxiliary toolchain list was rejected and corrected within round 3; validation
correction consumed no extra round. No app command was added to a preset,
Source rewritten or K weakened.

Search `search_690a9cdc20fbff89b13abcc21bd83b5d` consumed 3 rounds and 4
session exchanges, with 3 claimed attempts (2 executing builds), and completed
in 886.972 seconds. Original deadline `2026-10-02T08:28:14.985Z` remained
unchanged. D `fe4eb03a…` produced a fresh fully satisfied receipt and an
assessment-pending submission. All three durable result ACKs were confirmed,
metadata mismatches were empty and every Search reservation was zero. The
pilot's final ACK observer initially exited with a permission error after
Source completion; confirmation used saved records only, with no Requester,
Runtime, inference or execution restart.

The exact newly retained artifact (`3ad87f86…`, 798,720 bytes; retained ref
`5218c2dc…`) was then tested through the common retained Runtime at the same
code pin, as a separately authorized functional acceptance. It produced another
fresh same-K/D receipt, pasted a 186-byte SVG, downloaded an optimized 131-byte
SVG, toggled Multipass and reset options. Confirmed cleanup exited zero.
The contained page emitted service-worker sandbox errors and the browser
blocked one external request; these are preserved, so this does not claim
service-worker/offline behavior or unrestricted browser operation. Initial
browser fixtures failed on missing TMPDIR and Chromium Unix socket path length;
both saved same-K receipts and confirmed cleanup, and neither was relabeled
as a functional PASS. The successful fixture used an owned short `.tmp` path.
The standalone helper source/lock and binary hashes are in the evidence; its
new state field is explicitly `None` for this static-only observation.

## OCI single service

[OCI Source evidence](evidence/formation-oci-source-final-20261002.json)
(SHA-256 `71935dddaa3ccc7ce150c7056f3bd9cf37d2428649931b724e64f0c55caeff7c`)
records a new zero-known-D Search at the same code/API pins. Its verified root
Dockerfile declares a single BusyBox HTTP service, a digest-pinned base image,
literal `site/index.html` COPY, image-owned CMD and port 8080. The proposal used
the explicitly bound builder recipe, no runtime egress or state/variable grant,
and explicit logical HTTP Bind authority. The common builder produced an OCI
artifact and the normal contained Runtime issued a fresh same-K fully satisfied
receipt on its first execution. The proof retains base/completed image identity,
build provenance, network reports, execution facts and phase timings.

Search `search_ff3e9c7b9c810954c75573b4beede527` executed once, used 2 rounds
and 3 session exchanges, and completed in 262.384 seconds. An omitted required
empty `build_scripts` list was corrected within the first round before any
execution. After measured K success, the second round proposed no duplicate
build or execution. Original deadline `2026-10-02T08:49:28.841Z` remained
unchanged. Result delivery was durably ACKed, metadata mismatches were empty
and all reservations zero. This controlled representative fixture contributes
no additional unique OSS app. Compose, multiple services and source/Dockerfile
rewriting remain outside v0.

## Preserved changedetection infrastructure failure

[Initial final-pin path failure](evidence/formation-changedetection-path-failure-20261002.json)
(SHA-256 `f7d00a2a7bc863b0f41f6d73d5abada2b4b5d19cd7e18721cbe57517755920f4`)
records Search `search_24c0047081996252c75981adb6f985ec` terminating as
`infrastructure_failure`: Runtime admission hit the Unix socket path length
limit before execution. Its attempt was ACKed, original 08:55:31 UTC deadline
and used transfer/expanded budget remain recorded, and all reservations are zero.
No unchanged candidate was repeated or Source/K rewritten. A first short-path
fixture then failed to create a child of the root-owned parent before creating
any Search or attempt. Both directories remain preserved. The corrected run
uses a separately created owned parent and `/opt/ato/.tmp/fc3/r2`; it is a new
preregistered Search with the same Source/K/plan and zero-known-D initial input,
not a retry/resume that replenishes the failed Search's limits.

## changedetection.io Source regression

[Source native dependency evidence](evidence/formation-changedetection-source-final-20261002.json)
(SHA-256 `dc9dec18a70edab88d4015550df8b565cb3508cebd95e88b38a14fe54317c2d4`)
records corrected infrastructure Search `search_6eb04bad8d5678c5d01b4990e3faa338`.
The proposal inspected the wrapper declared by root setup/Dockerfile, followed
its explicit `changedetectionio` import, and used Source-documented datastore,
host and port arguments. Source-owned requirements used generic
`python_build_requirements`, Python 3.12.7, explicit GCC 13.3.0/Make 4.3.0/pkg-config
1.8.1 and isolated setuptools 83.0.0/wheel 0.45.1/packaging 25.0 build dependencies.
Only PyPI HTTPS was granted for dependencies/build; Runtime egress was denied.
The operation built feedgen, jstyleson and websockets from sdists, saved 168
wheel identities/hashes/provenance, and installed completed wheels offline.

The first actual execution produced fresh same-K PASS for D `6c934956…`.
It consumed 2 rounds and 4 session exchanges, including two inspections within
round 1, and completed in 592.080 seconds. The original 09:06:40 UTC Search
deadline was retained. Source result/BuildRecord/retained publication were
ACKed, metadata mismatches empty and Search reservations zero. Stored usage was
306,003,211 bytes; measured Source input expansion was 16,887,438 bytes. The
separately retained artifact is 167,289,522 bytes with 419,612,170 expanded bytes;
these distinct counters are not treated as the same measurement. The corrected
run followed a preexecution infrastructure failure; it is not represented as an
uninterrupted first-Search success or another unique OSS app.

[Current-pin functional/state evidence](evidence/formation-changedetection-functional-state-final-20261002.json)
(SHA-256 `174643c9822c7d945481fcede1c1944ca92df86983b2c3d33ea5a6a94b90b833`)
records two separately assigned common Runtime Runs using the exact newly
retained artifact. Both issued fresh fully satisfied receipts for the same K/D.
The real browser created an owned watch, edited its title and paused it. After
confirmed stop, writer fence 1 committed revision
`isrev_01M3XX7R8X44PAKN3NEJ5Z57M6`. A new Run restored that exact revision under
fence 2, verified the title and paused state, deleted the watch, confirmed stop
and committed child revision `isrev_01M3XXD0KF5M448XN0FXRV0KE4`. The state slot
ended at writer epoch 2 with no active writer or quarantine. Runtime egress was
zero. External site fetch was denied and is unverified.

The first functional observer timed out before readiness, then queued confirmed
stop; its receipt/state commit and writer release succeeded, but no browser was
started and no UI PASS is claimed for it. That failure remains in the evidence.
The successful separate fixture changed only the observer readiness window to
220 seconds; each helper retained its original 300-second execution allowance.
The closed Source Search was never resumed. Production StateService assignment,
artifact, commit and fencing routes are exercised through a private filesystem
dispatch fixture; automatic production Source state provisioning remains pending.

## Remaining acceptance and accounting

The separately preregistered [Kutt Source regression](evidence/formation-kutt-source-final-20261003.json)
(SHA-256 `475852aebde08c8f486d152a0b9d0695f3763d958ac0099e406937f57b64c875`)
completed at Ato `da7c452c` / API `9b483d4a`. It began with zero known D,
inspected eight files through root declarations and explicit imports, and used
three rounds / seven Codex exchanges in 769.653 seconds. Initial native audit
refused missing locked `msgpackr-extract`; round 2 added it to the same generic
offline rebuild alongside `better-sqlite3`. Source-owned migration ran inside
the contained Runtime. A Source-backed GET guard observed root 302, then the
typed owner operation used variable references and observed POST 201. Temporary
JWT binding remained a private Runtime grant. D `ffcbe50c…` produced a fresh,
fully satisfied receipt for unchanged K `2940ae2d…`. The third round proposed
no additional execution after observing this success. Submission remains
`k_reached_awaiting_assessment`, with no normal Run permission or deployment.

Both attempts were durably ACKed under fence 1; claim, Source acquisition and
result transport policies all retained `max_retries = 3`. Candidate stop was
confirmed, metadata mismatches empty, byte/attempt reservations zero and both
registered owner values revoked with encrypted-value count zero. Original
Search deadline `2026-10-03T05:06:04.881Z` was unchanged. Structured network,
execution facts and source/dependency/build/launch/verification/cleanup timings
are retained in the acknowledged Runtime result records. The ready retained
artifact has 67,475,730 verified bytes and 181,518,590 expanded bytes; these
are distinct from Source input expansion and Search stored-byte usage. No new
BuildRecord upload is claimed for this Search (`build_record_ref = null`).
Functional/persistence evidence retains its separate earlier pin and artifact.

Producer frames and public measurement records contain no owner value body;
the password scan covered 882 public files. This statement is scoped to those
records: inspecting the controller source exposed its fixed nonsecret local
verification email to orchestration tool output. It was not an external account
or secret, but whole-orchestration value non-disclosure is not claimed. A future,
unexecuted controller template generates that email privately. The measured
controller, original Searches, limits and evidence were not changed or rerun.

The Kutt Source regression at the earlier pin stopped without PASS. [Preserved failure](evidence/formation-kutt-reasoning-budget-failure-20261003.json) records a correct pre-launch native audit refusal for missing `msgpackr-extract` lifecycle processing, followed by exhaustion of the frozen six-exchange Search allowance. The original code incorrectly reported the local pre-dispatch exhaustion as `provider_error / infrastructure_failure`. One attempt was ACKed, all reservations were zero, owner values were revoked, and no receipt was issued. The closed Search is not resumed or reset. The common terminal correction is implemented in Ato #1476 / API #727. A separately preregistered new Search uses eight Codex exchanges; its original three-round, 10-minute round / 30-minute Search and retry limits remain fixed.

[Actual pre-dispatch budget control](evidence/formation-pre-dispatch-budget-control-20261003.json)
(SHA-256 `3be6a33a946e4f0e3ffa47c8d508967b9b1d2bb1f6180ec0fa2a7b5ced320055`)
consumes six deliberately invalid repeated inspections in a negative fixture.
It ends as `budget_exhausted` in 2.212 seconds, creates no seventh input,
provider-call row or Runtime attempt, and leaves all reservations zero.
The legacy round status remains `provider_error` with authenticated
`call_budget_exhausted` pre-dispatch evidence; the shared Rust Search authority
owns the terminal classification. No actual Requester restart is claimed;
saved-exchange restart/recovery is covered by the Worker unit test. Three
receiver startup failures and the final observer's pathname/JSON error remain
preserved. The valid result was collected from saved records without rerunning.

[CI comparison](evidence/formation-budget-terminal-ci-comparison-20261003.json)
records the new Ubuntu clippy warning and its correction, Ubuntu CI PASS at
`88ee365c`, a passing local full
CLI clippy check, and the same 13 Activity/CORS failure names on merged API main
and the new API head (155/168 PASS each). Local Node 25.6.0 differs from CI Node
22; Linux WebSocket handshake and the macOS worker-start failure remain
unreproduced CI limitations. These failures are not billing failures or green CI.

 [The interrupted prior exchange](evidence/formation-kutt-unanswered-round-20261003.json)
(SHA-256 `af14132136eb99ff85201731ce197a6330f8773b6fa6a038876874f641df2bfd`)
closed Search `search_9a1e27fd1a9d5102a86620b2d61e7b93` at its original
10-minute round deadline before creating any candidate or Runtime attempt.
Its original Search creation/deadline remain recorded, all reservations zero,
and both owner values revoked. It is preserved as a failed measurement and
never resumed, refunded or reset. No old WBO UNKNOWN was reexecuted.

[Final HTTP input controls](evidence/formation-bound-http-controls-final-20261002.json)
(SHA-256 `f6aecff9ee3ccd2e0fba6e1807861d04acee9531a09d6cfa06ac569d6412725f`)
use actual Coordinator/Runtime at the same pins with fixed Source exchanges.
Scope-matched reusable input produced a guarded POST 201 and fresh same-K PASS.
Out-of-scope input returned `needs_input / scope_mismatch` before execution;
its original 30-second clock then produced a deadline result without execution.
A deliberately lost POST response produced
`source_runtime_http_operation_failed`, one POST request observation, no fresh K
receipt and no additional attempt. Each claimed ticket retained custom
`max_retries = 1`; all results were durably ACKed, candidates confirmed stopped
where execution started, byte reservations zero and encrypted values revoked.
Public value occurrences were zero. These are controlled fixtures, not provider
or unique OSS successes. An initial disk preflight rejection occurred before
Search/Runtime creation and remains preserved.

[Final input lifecycle controls](evidence/formation-input-lifecycle-final-20261002.json)
(SHA-256 `a07e694a17af3fde4c9a82b5d970943c5932c39dd51a317fcceb6a07e25b257d`)
measure expired and revoked values, and two matching reusable candidates.
They report `expired`, `revoked` and `ambiguous_scope` respectively before
execution. All retained original creation/deadline and attempt usage; each
post-deadline input request returned HTTP409 without Requester resume or new
execution. All result ACKs and zero reservations were checked, registered values
were removed and public scans found zero protected value occurrences.

[Final embedding controls](evidence/formation-embedding-controls-final-20261003.json)
(SHA-256 `5d1650010c7629f3f980615297c92a40115f69ffa60e1b1b6e505eda4b84854c`)
use the same Source/K in two new fixed-exchange Searches at the final Runtime pin.
Source-owned Node build succeeds with nonsecret configuration, but forbidden
embedding is rejected as `secret_artifact_embedding_refused` before publication
and reaches the next reasoning input. Explicit owner permission for nonsecret
embedding allows a fresh same-K PASS and assessment-pending artifact. Neither
permit exposes the value in a model, D or public JSON/log. Both results were
ACKed and encrypted values removed. The permitted artifact is excluded from
the public evidence scan. Permission-change resume and PWA UI are not measured
by this pair; earlier UI records keep their own pins.

SVGOMG, OCI, changedetection.io and Kutt Source regressions are complete at the
explicit individual execution pins above;
changedetection.io functional/state verification is also complete within the
explicit private acceptance boundary above.
Current-grant Kutt native/migration/function/state/restart evidence is
[recorded separately](formation-state-verification-input-grants-20261002.md).
Fixed-artifact functional/state arms do not count as independent Source
exploration, API producer success or extra unique apps.

The historical unique OSS totals remain Codex 3, API 0. SVGOMG is a repair
PASS, not an initial PASS. This regression performed zero paid API calls;
actual Codex provider calls, token usage and cost remain unavailable rather
than fabricated from session exchanges. The small paid API allocation remains
1 used of 24 (23 remaining); aggregate historical/current reservations count
18 of 41. Estimated spend remains $0.082825, remaining $0.482081 and unsettled
reservations zero, including one HTTP401 charge settled conservatively at
$0.017204 with actual usage unknown. A valid key for the same authorized account
is still required for independent API acceptance. Old WBO UNKNOWN and old
results are preserved; no 100-app remeasurement, deployment, remote migration,
feature flag or ordinary Run permission has been performed.

[Independent API preparation](evidence/formation-independent-api-preparation-20261003.json)
retains four unexecuted plans at Ato `da7c452c` / API `9b483d4a`, with the same
rooted Source/K as each Codex arm and no successful D seed. SVGOMG retains its
explicit custom retry 1 control; the other arms use default retry 3. Call caps
match the corresponding Codex arms (6/6/6/8), while the aggregate historical
cap remains 41 and the small allocation 24; these are not four separate new
24-call grants. The original five journals and prior HTTP401 journal were read
through the product preflight: 18 slots, estimated $0.082825, unsettled zero.
Duplicate archived exports are excluded. Every prepared plan is blocked before
Search creation until a valid same-account private credential is supplied.
The known old remote credential file retains its September 22 timestamp and
was not read or retried. Credentials are never placed in plans or evidence.

Current [official pricing](https://api-docs.deepseek.com/quick_start/pricing/)
was checked on October 3: the retained reservation uses the Flash peak ceiling
($0.30/M input cache-miss, $1.20/M output); off-peak rates are half. This retains
the authorized worst-case bound of $0.395692 for 23 remaining calls. Token-based
fee estimates are not asserted to be invoice charges. Before another run,
credential and current-price checks must still be confirmed against that run's
frozen plan.

After preserving the Kutt result, free disk fell below the unchanged 20 GiB
preflight floor. Only 853 inactive, single-link compiler `.rmeta` files in this
task's target were removed (631,881,065 logical bytes). Frozen 3bb/da7 binary
hashes remained unchanged and free space became 22,058,913,792 bytes. The full
owned cleanup manifest is preserved remotely as
`post-kutt-owned-compiler-cache-cleanup.json`, SHA-256
`0b3187d8f9c1a4ee81b9352024c753f57db36ef203ef6053cfeabee185d5ad05`.
No Source, measurement, Runtime, retained artifact or other checkout was removed.
Both owned acceptance Coordinator ports were closed; the three prior shared
Runtime PIDs were left running.
