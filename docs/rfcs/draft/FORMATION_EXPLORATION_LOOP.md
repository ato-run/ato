# D-conversion repair pilot (unmerged)

The first 100-app exploration arm added no generated-D PASS. Its preserved
diagnostic append is `docs/ops/formation-exploration-diagnostics.{json,md}`.
The following changes are bounded repairs for a separate small OSS pilot;
they do not reinterpret the historical measurement.

- Optional `static_output` in `execution_plan@1` connects a Node source-owned
  build to the existing `ato.browser@1` serving adapter. Entrypoint is a verified
  package.json ref; the output is workspace-relative. No process argv, module,
  state or environment accompanies that serving route. Frozen K and explicit
  port bind requirements remain authoritative. This makes a buildable frontend
  expressible without running browser scripts as an HTTP server.
- Initial four-file context chooses distinct source roles; configuration files
  precede arbitrary source and test entries do not displace app startup files.
  A later inspection keeps initial context in spare slots, within the same
  16 KiB total and 8 KiB per-file prefix bounds. No new private paths or
  authorization map enters provider requests.
- Optional `unsupported.reason` uses bounded reason codes. Legacy declines
  retain their canonical bytes; new raw evidence distinguishes capability,
  binding and source-information reasons. A reason is a model claim, not proof
  of runtime capability or impossibility.
- Two adjacent completed empty declines, without inspection or validator
  feedback, stop `no_progress`. Provider failures retain their separate round
  accounting. The effective default limit remains three and no budget resets.
- RequestEvidence optionally records actual transmitted source IDs, prefix
  digest/length/truncation and bounded failure/previous-D/validator codes after
  provider input trimming. It does not store text, log/environment values,
  credentials, headers or reasoning. Exact request/body hashes still bind it.

Prompt v4 is separately frozen for the pilot; prompt versions 1–3 remain
unchanged. Model, source pins, K, Runtime ceiling and round limit are not tuned
between applications. Direct PASS and failure→repair PASS are reported
separately. No production execution/approval/deployment follows a submission.

# Bounded Formation exploration with post-submission assessment

Status: draft implementation contract for the explicitly requested exploration arm.
Existing baseline, legacy provider arms and historical ledgers remain separate.
Actual provider/Runtime evidence is recorded separately in docs/ops; this design document does not imply deployment.

## Frozen boundary and changing Derivations

The requester freezes K, verified source closure, Runtime constraint, exploration
ceiling, provider configuration, journals and all resource budgets before search.
The external worker config supplies an independent operator ceiling. Every ticket
must fit both ceilings. Proposal text cannot amend either frozen policy.

Canonical D includes phase-scoped network requirements and logical authority
requirements (protocol, resource, operation, phase). Changing requirements changes
D's digest. Physical gate sockets, base archive paths, credentials, temporary state
and owner management authorization are bindings; they are not reusable grants.

The search tries declared reusable D first, with the existing attempt budget.
Then a typed CandidateProducer plan is validated and compiled by Rust into D,
admitted by the registered adapter and executed by the common Runtime. HTTP
observations are compared against frozen K by the existing Verifier. No model
output is passed to a shell or allowed to claim a verification verdict.

## Round and continuation invariants

`formation.max_rounds` is a positive integer, default 3, captured in the frozen
search. One opened round owns at most one candidate. Decline, invalid output,
inspection request, timeout and provider failure consume that round. Known-D
attempts and generated rounds are separately reported; actual execution still
consumes the shared attempts, time, transfer, expansion and storage budgets.
Dependency, build and runtime egress use separate phase gates and a shared
non-resetting transfer reservation. Permission changes and reduction attempts
consume subsequent rounds, never an implicit unbudgeted retry.

Earlier rounds, generated D and inspection counts remain durable. CAS/fences
continue to govern single round claims, completion and attempt results. Pending
attempts and unresolved external-effect UNKNOWN prevent another attempt. Model
transport with unknown usage is conservatively charged its full reservation and
halted; this accounting action does not resolve a workload UNKNOWN.

The same D is not executed again without a newly admitted retry basis. Repeated
candidate digests with no progress end the search. This initial adapter does not
implement automatic retry of identical D. Restart reopens the same journal and
search instead of assigning a fresh budget or source context.

## Proposal and physical adapters

`execution_plan@1` exposes bounded source IDs/digests and public purpose hints,
listed toolchains and, when configured, a frozen source-OCI recipe. Private source
paths and resource bindings remain in the requester. Source text is verified,
credential-excluded and bounded by the existing 16 KiB projection ceiling.
Inspections refer to already authorized immutable files and consume a separate
count; they do not execute arbitrary commands or expand the domain.

Registered operations initially include pinned Python/Node process startup,
Python hashed binary-wheel requirements, npm locked install with scripts disabled,
existing npm build script names, and the existing isolated source-OCI builder.
Literal interpreter argv, relative cwd, port, isolated filesystem slots and public
environment values are typed and validated. Unrepresented runtimes or unavailable
physical adapters have concrete diagnostics; proposal capability is not an
execution success.

Process execution uses bwrap network namespaces plus Landlock, guest-local
readiness and declared ingress, and owner-run phase-scoped HTTPS CONNECT gates.
No guest gets host network, management socket or production state. Public DNS is
rechecked by the broker; private/link-local/metadata endpoints are refused.
The source-OCI route uses frozen base archives, existing Dockerfile validation,
a private builder daemon, its existing resource limits and verified image
materialization. It preserves the current Cmd/state/file-capability profile.
The common OCI runtime uses an explicit operator socket and a fresh empty Docker
config instead of ambient Docker contexts or registry credentials. K produced by
the builder is not substituted for the exploration K.

## Success, reduction and later authorization

A fresh fully-satisfied receipt assigned to exact K/D/attempt establishes
`k_reached_awaiting_assessment`. The coordinator stores canonical K/D, receipt,
requirements and round history as an unapproved submission, not a normal
verified-route authorization. Normal Run, retained replay, publication and
production deployment cannot inherit the exploration grant.

Remaining rounds may propose a strict requirements subset with identical source,
steps and other execution semantics. A fresh PASS replaces the best submission.
A failed or unverified reduction retains the preceding successful D and receipt.
The result reports attempted reductions and their verification; it makes no claim
of mathematical minimality. Later risk assessment and owner authorization must
bind the submitted D digest in their own workflow.

## Provider accounting and acceptance

Both transports reserve and fsync before credential access or sending. Model,
prompt, limits, pricing and journal bindings are frozen. Singleton choices bypass
the DecisionProvider. Restart uses assignment-checked cached answers; no unresolved
send is retried. Responses record model, usage, latency and estimated cost.
No-call reasons and full reservation charges are recorded separately from actual
known token usage. Journals never contain credential values.

The execution gate requires actual Coordinator, contained Runtime and live-provider
acceptance on small HTTP application fixtures, including evidence-based repair,
ceiling refusal, permission reduction, failure retention and restart. Frozen upstream
pilots are measured separately. Fixture PASS never establishes upstream application
or functional acceptance and is never counted in the 100 OSS arm. The frozen cohort
is measured only after the actual execution gate; old results/policy conditions remain
unchanged. WBO/copyparty pilot failures stay failures without prompt/source changes.