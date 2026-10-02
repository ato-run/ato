# Private functional acceptance with current Formation input grants

The retained functional verification helper now accepts an optional
`formation_input` with a private `token_file` and the expected `search_id`.
It claims a current Coordinator attempt through the existing durable Worker
delivery path, matches the retained descriptor against exact K/D, base K,
browser Contract and Search, and redeems the canonical input requirements
through the production encrypted store/assignment/grant service. Values remain
private `ResolvedVariable` inputs; the helper does not synthesize grants or
read stale values from the earlier successful Source Search.

This path requires a separately preregistered functional verification Search.
Its admitted Source-backed D must match the already fixed retained artifact.
The original Source acceptance is closed and never resumed or reset. Retained
D reuse in this functional arm is explicit; it is not an independent API
Source-exploration arm or another unique app success.

The same isolated private Coordinator bundles the production Runtime Network
and Runner lease-state routes. Only its mode-0700 filesystem channel can invoke
the fixture dispatch service; the front HTTP port still rejects that control
path. D1/R2 and the input encryption master are local. No deployment, remote
migration, feature flag or ordinary Run permission is created.

The helper's `--advertise-bound-inputs` mode measures actual local process facts
and uses the same containment/current-client predicate as Worker admission. It
declares neither OCI nor an unbound input path. Execution requires the existing
contained retained realizer, denied Runtime egress, current input grants and an
authenticated revision-backed state attachment. It passes both bindings into
the common Runtime rather than copying HTTP, verification or state semantics.

The absolute execution deadline comes from the currently claimed ticket, not
from the helper's start time. Preparation uses a borrowed execution view of the
existing `LeaseStateArtifactTransport`; every writer acquisition/artifact HTTP
request takes its timeout from the same remaining execution allowance. The
explicit request ceiling is narrowed. Commit, release and quarantine delegate
to the reporting transport and continue after expiry. The historical input-free
functional mode retains its separate 300-second private verification allowance.

Before any workload starts, failure can abort its known idle state writer.
Once execution may have started, an uncertain stop keeps the writer quarantined.
Confirmed stop precedes state commit and writer release. State commit does not
depend on result connectivity: an already produced common Runtime receipt is
reported afterward through the existing durable result path and ticket retry
policy. Phase measurements use the shared control, including its existing
cleanup timer, without granting new execution.

`--report-saved` only sends the recorded ticket/result through that same durable
delivery directory. It does not claim another attempt, redeem inputs, restore
state, run the app, restart reasoning or replenish a retry/round/budget/deadline.
Exhausted delivery remains exhausted and its result remains preserved.

Validation so far: private helper compile/clippy with warnings denied and the
origin-boundary test passed; the complete Worker library suite passed 79 tests;
state artifact tests passed 12, including an actual socket boundary proving
that expired preparation sends no HTTP request while writer release still
works. The exact API receiver bundle built from
`02a9e58bbf2f8b27c6f976107995c5c14f662a8f` and includes all three required WASMs.
The pinned Linux helper also compiled successfully and passed its one origin test;
the 12 state transport tests passed on Linux. The architecture suite passed
43 tests. CLI, Worker and the preflight helper were rebuilt from code pin
`3bb28192419f4a12cf2566cab57a8f641cf88018`.
The initial standalone helper lock needed the newly referenced HTTP Adapter;
its old lock and initial compile failures are retained. A missing visibility and
non-Copy record borrow were corrected before the successful checks.

The complete connected Worker clippy command with warnings denied fails on
`runtime_launch/lease.rs:736` (`ActiveWorkload`, `large_enum_variant`). The
exact PR base `f362c894` fails with the same diagnostic; this unchanged enum
was not suppressed or refactored. The private helper clippy passes.

[Platform comparison evidence](evidence/formation-platform-ci-comparison-20261002.json)
records Windows cross-checks with isolated head/base outputs: both report
13 errors from unconditional Unix browser sandbox APIs and the non-Unix
`plan.rs` branch's missing `bail` import. This is compilation evidence, not
execution on a Windows Runner. An initial shared-target head check reused
stale base Formation artifacts; it is retained and excluded.

On macOS the exact HTTP PR base's process ownership test passes in isolation
and in the full portable suite, but the suite fails one static cleanup test.
The head CI's process startup failure remains unreproduced. API browser
checks at head `02a9e58b` and base `f83d1109`, using the same local Node
22.22.0/browser dependencies, both fail at `run.mjs:310` (404 instead of 403).
Both also emit the existing `nodeCrypto.randomBytes` warning. The CI uses
Linux/Node 22.23.3 and fails earlier on the WebKit presence handshake; that
exact failure remains unreproduced. These jobs are not reported green.

[Kutt functional/state evidence](evidence/formation-kutt-private-functional-state-20261002.json)
is now complete at helper/CLI pin `3bb28192` and API pin `02a9e58b`. Both
separately assigned Runs produced a fresh fully satisfied receipt for the same
K `2940ae2d…` and D `1c5a59c4…`. The first used writer fence 1, created an
owned local account API key and owned link, stopped, and committed revision
`isrev_01M3XSPYG2HFG55PTZ2Y27ZBPN`. The second used fence 2 and authenticated
restoration of that exact revision; it logged in without registering the owner
inputs again, authenticated with the persisted private API key, found and
deleted the same owned link, and committed child revision
`isrev_01M3XSSVX05DEQ9VWX60AEWK8S`. The old login JWT cookie was rejected
after the fresh Search's signing binding changed.

The existing Source-backed HTTP status guard performed bootstrap only for the
initial Root redirect; restored Root response skipped the non-idempotent POST.
K was not weakened. The Source-owned migrations and native SQLite dependency
were exercised through the ordinary contained retained Runtime. Network reports
show no candidate egress. Both results were ACKed with no metadata mismatch,
all Search reservations were zero, state writer epoch was 2 with no active
writer/quarantine, and all registered owner inputs plus temporary signing
values were revoked through production ownership routes (encrypted values 0).
Public JSON/log scans found zero occurrences of protected email, password,
login JWTs or the generated local API key. Values were not sent to a model or D.

This is explicitly a fixed-artifact functional arm, using fixed Source
inspection/proposal exchanges for current admission and grants. After receipt,
confirmed stop, state commit and ACK, each Requester was ended without seeking
another Source artifact publication; the recorded Source view remained
`running`. No Source exploration completion, independent API producer success,
additional unique app or ordinary Run permission is inferred from it. Each
functional Search keeps its original clock and budget; the closed original
Source Search was never resumed. GeoIP analytics and other app features remain
unverified.

The evidence preserves all prior fixture failures: waiting for `reserved`
instead of `pending`, the partial checkpoint read, Unix socket length and
untraversable workspace/shim paths, observer heartbeat HTTP503, the link DTO
shape, publication waiting, and incorrect bearer authentication (the initial
link in that trial was anonymous). None was relabeled as an owned-link PASS.
The claimed socket-path trial reached actual Coordinator UNKNOWN after its
original 07:46:20 UTC Search deadline. Its checkpoint stayed byte-identical, no
Requester/Runtime/inference/execution was restarted, and the conservative
512 MiB expanded/stored usage stayed charged with zero reservations. Earlier Kutt Source same-K repair PASS remains pinned separately in
[the HTTP acceptance record](formation-bound-http-port-acceptance-20261002.md).
The [current-pin changedetection functional/state proof](evidence/formation-changedetection-functional-state-final-20261002.json)
also records fresh same-K/D receipts, browser watch creation/title edit/pause,
confirmed stop, exact revision restoration with fence 2, preserved title/pause,
deletion, child commit and writer release. It uses the input-free helper's
separate 300-second acceptance allowance; external fetch remains unverified.
The earlier changedetection state proof and old WBO UNKNOWN are preserved. This
helper does not implement automatic production Source state provisioning or
independent API acceptance. No external account authentication or paid API call
was performed; the approved small API call ceiling remains 24 with its old
ledger intact.
