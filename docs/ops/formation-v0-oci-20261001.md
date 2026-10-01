# Formation v0 OCI proposal integration — 2026-10-01

Depends on the control change in Ato #1453. The fixed merged main remains
`8fca0f4b78b1bed92eae21823e5c5f7207058271`.

Typed execution plans can select an authorized digest-pinned OCI image or a
source-owned Dockerfile. Root Dockerfile is directly selectable; a nested file
must be literally referenced from root declarations, following at most 32
credential-filtered files and 64 KiB of captured text. Symlinks, unreferenced
files, source rewriting and multiple service orchestration are refused.

Both providers receive the same capability projection. A configured recipe is
not an available builder: the worker measures its bound executable, private
builder tools, delegated cgroup constraints, native platform, approved archives
and bound Runtime daemon. Owner paths and management sockets stay private.
Builder absence and temporary Runtime absence are distinct decline reasons.

Existing immutable base-image graphs, source closure, Dockerfile selection,
output image/archive identities and build provenance are retained. Image
acquisition uses an approved verified archive with no acquisition network;
source builds use their explicit scoped HTTPS allowance, private daemon,
cgroup/filesystem limits and isolated network. Workloads use the ordinary OCI
adapter with a read-only input and explicit writable state. The adapter receives
neither management socket nor host credentials. OCI variables/egress remain
unavailable until the dependent binding change implements their actual path.

Archive reads, materialization, Docker commands, readiness, preflight DNS and
build logs are bounded by the original execution deadline. Process-group
termination also bounds inherited stdout/stderr pipes. Cleanup has its own
bounded allowance after expiry; uncertain workload cleanup remains UNKNOWN.
Build/launch/Contract failure feeds the ordinary exploration evidence loop.
Every successful candidate still requires the original K and a fresh Runtime
receipt and remains awaiting risk assessment.

Validation so far: macOS OCI/Worker/portable libraries passed 31/62/94 tests;
29 proposal integration tests passed; related clippy with all targets/features
and warnings denied passed on macOS and Linux. Linux OCI/Worker/portable libraries passed 31/62/94 tests. Full workspace
validation is in progress; its results are retained in the task worktree and
updated in the pinned acceptance report. No actual OCI acceptance or provider call has yet been executed.

No deployment, remote migration, flag change, normal Run authorization or
publication occurred. Old measurements and WBO UNKNOWN are preserved.
