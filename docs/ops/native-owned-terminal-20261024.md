# Owned Native Terminal execution — 2026-10-24

This is an isolated CPU Linux Docker acceptance of a guarded execution primitive.
It is not a shared Hosted Runner/staging deployment, public launch capability,
Formation end-to-end completion, GPU allocation or operator-key test.

## Observed execution

The digest-pinned, already-present Python CPU image ran the declared entrypoint
as numeric UID:GID 65532:65532, with a readonly workspace/rootfs, 128 MiB memory,
500m CPU, 32 pids and internal network. No shell/Agent/helper was assumed in the
image. The owner held Docker attach, a private PTY/UDS, the common native probe
and the existing scoped Surface gateway. Directory-fd aliasing also exercised
the long workspace path without changing D/K.

The actual PTY returned `go\r\n1\r\nEND`. Repeated GETs returned these same
frozen bytes. The existing Rust HTTP K harness accepted exactly that observation
on the declared probe Port and rejected the wrong Port and readiness text;
original ContractRef remained unchanged. This receipt is a local test harness
result with an explicit target, not a Hosted acceptance claim.

The next viewer input returned `2`, proving GETs had not replayed the action.
The viewer resized to 120×40 and a reconnect observed that size from the actual
workload OS terminal. Invalid assertion, wrong Origin and valid-MAC wrong Run
were refused. Actual observed kernel principal UIDs were all 65532. First
ready+capture elapsed samples measured 768ms and 834ms on this host; these are
individual CPU measurements, not statistics or staging latency.

Additional actual Docker tests proved owner EOF after the workload had accepted
input during a stalled 30-second action, owner EOF after startup, idle expiry,
maximum lifetime, workload exit and startup timeout. Each required confirmed
termination, closed private endpoints, removed socket and no owned container or
network after the existing exact-lease recovery sweep. Preflight tests refused
bad key/Origin/scope, missing Port facts, unsupported guest TCP promises, wrong
selected D and wrong owner before runtime files or OCI creation.

## Reproduction

Use a fresh directory under the workspace for TMPDIR and ATO_NATIVE_TEST_ROOT,
set a sufficient test file-descriptor limit, and explicitly select an already
present digest-pinned Linux Python image as ATO_NATIVE_TERMINAL_TEST_IMAGE.
Then run:

```
cargo test --locked -p ato-connected-realization-worker runtime_launch::native::tests -- --include-ignored --nocapture --test-threads=1
```

The actual Docker cases are ignored by default because they require that
explicit image and a real Linux Docker host. They were explicitly executed.
No image pull, unrelated container change, paid GPU or production key was used.
Normal Process regressions require the actual built worker sandbox binary via
existing ATO_TEST_WORKER_BIN. Omitting that build caused the first `policy`
libtest error; the correct harness passed on both the base and this branch.
The initial regression run also exposed the host's low fd limit and transient
ETXTBSY in an existing fake-Docker script; serial execution with the required
limits passed. No test or runtime workaround was added.

## Remaining integration gates

- connected Runner verified_spec still returns native_surface_unavailable;
  ordinary OCI conversion and generic attempt admission remain closed.
- execute_runtime_launch/published_endpoint_name and active serving assume an
  ordinary Web endpoint. Host-owned Native has no guest endpoint and needs a
  separate branch using the exact selected descriptor and owned private address.
- reuse the existing Run/lease/Runner/Port/path-scoped HTTP observation ingress
  and Hosted Rust verifier; public readiness must await its real K result.
- preserve existing writer prepare/abort/commit/quarantine and resource journals;
  this primitive's owner channel is lifecycle control, not new authorization.
- generic native_surface_probe_v1 alone cannot advertise Terminal-only support
  as Pixel support. Negotiate kind/profile/transport with implemented backend
  capabilities before changing API admission or Runner advertisement.
- trusted GUI producer, shared Runner installation, staging launch/reconnect/
  persistence acceptance and receipt-gated public Native publication remain.

The Docker transport details use upstream [start](https://docs.docker.com/reference/cli/docker/container/start/)
and [dial-stdio source](https://github.com/docker/cli/blob/master/cli/command/system/dial_stdio.go).
The fixed resize operation uses the existing Docker Engine container resize API;
no general daemon proxy is exposed to an application or viewer.

## Validation record

Linux five-package library regression: 548 passed (OCI 37, connected Runner
151, IPC 79, portable application 111, runtime-attempt 170). The subsequent
native descriptor helper added one passing test. All five actual Native tests
passed, including the four deliberately ignored Docker cases executed explicitly;
11 Hosted-validator fixture integration tests passed. Combined this is 564
distinct passed tests, with six other pre-existing opt-in cases unexecuted.
All-target `--locked` clippy for the five packages, architecture-check (44
workspace packages), format and diff checks passed. The final bounded attach
cleanup/creation deadline changes reran OCI and actual Native cases.

Relevant isolated logs live under the owning workspace `.tmp` directories on
the CPU test host; they contain no credentials. No capability, API guard,
feature flag, installed Runner binary or staging job claim was changed here.
