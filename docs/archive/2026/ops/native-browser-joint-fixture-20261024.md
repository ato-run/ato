# Native browser joint fixture — 2026-10-24

> Status: historical evidence / past plan. Language: en.
> Preserved content; relative navigation updated. Current [documentation map](../../../README.md).

## Scope

This test-only child freezes #1542 plus #1541 at combined base
`a322c898dce513ca2d5a412ea1a0bb7476d44f96`. No production sources, Core/K/D,
identity, API, authentication scheme, Runner flag or deployment are changed.
The original verified common helper and real OwnedNativeRun are reused; no
second/manual display server is created.

The ignored test is
`native_dispatch_tests::real_host_boundary_native_pixel_browser_joint_fixture`.
It uses the existing fresh root ConnectedWorker::new acceptance, HostBoundary
UID/GID65534, verified immutable helper, independent GUI, original authored K,
read-only asset input, stop output save, credential canary and existing owned
resource recovery/cleanup. Test control-plane/grants remain fixtures. Docker,
X11, RFB, gateway, bytes and cleanup are real, not Hosted API/R2 or staging.

## Coordination contract

Run in a fresh process with explicit fixture image/archive paths. The new case
requires a new `ATO_NATIVE_BROWSER_FIXTURE_DIR`; Rust creates it mode0700.
`ATO_NATIVE_BROWSER_ORIGIN` defaults to `https://127.0.0.1:4179` and permits only
an explicit HTTPS IPv4 loopback port. Existing cases keep their previous origin,
key, 30-second policy and behavior. This new test-owned Run declares max90 seconds
through the existing wire and arms an independent absolute90-second clock just
before execution. Startup, ready ACK, idle, stop and other Runner budgets are
unchanged. Reconnect never renews the fixture expiry.

Only after the original K passes and the ready HTTP ACK is written does a held
owned test thread publish create-new atomic `ready.json` mode0600. It contains
version1, remote loopback gateway URL, origin, actual session/surface/execution
IDs, fixture principal, fixed expiry, initial640x480 dimensions and absolute
`checkpoint_file`/`checkpoint_result_file` and `done_file`/`result_file` paths. Tokens and keys never enter metadata. The Node
peer signs fresh unique-jti fixture assertions with the existing audience/kid,
clamped to min(now+60, absolute expiry). It forwards actual browser Origin;
localhost SSH forwarding is supplied separately, never by rewriting metadata.
The browser uses an isolated test clipboard stub, not the OS clipboard.

The sole accepted normal done record is exactly:

```json
{"version":1,"action":"verify-and-stop","expected_clipboard_latin1_hex":"636166e9090a","expected_width":1024,"expected_height":768}
```

Those request values must match Rust constants; they do not supply expected
state or execute commands. Done is bounded to1024 bytes, owned by the fixture
directory's UID, mode0600, regular and opened O_NOFOLLOW/O_NONBLOCK. Unknown,
duplicate, missing, oversize and wrong-value fields reject immediately. Root
publishes a new done file atomically as its owning UID. `{version:1,action:abort}`
is deliberately a rejected record that triggers early failed-stop; it is not a
second accepted command or a successful outcome.

Before resize/reconnect/Stop, the browser publishes the fixed request below and
waits for the correlated checkpoint ACK. The request uses the same private,
bounded, no-follow, create-new policy as done. It cannot supply expected state.

```json
{"version":1,"action":"verify-clipboard","expected_clipboard_latin1_hex":"636166e9090a"}
```

Rust requires its isolated authenticated X11 selection to be actual
`Some(caf\xe9\t\n)`, then uses a separate permission=false paste connection,
requires it to close and confirms the selection remains unchanged. The exact
12-field ACK carries version/status, the five ready identity fields, the same
absolute expiry, actual clipboard hex, clipboard-checkpoint and paste-denial
facts, and error. Browser wire bytes alone cannot satisfy this checkpoint.
Normal done without a successful checkpoint rejects. Invalid input or an actual
selection mismatch produces failed ACK/result and bounded stop.

After normal done, Rust records the post-disconnect selection separately; None
is permitted only after a prior matching actual checkpoint. A permission=false
resize connection must close without changing that observation. A fresh
permitted connection checks current dimensions1024x768 and absence of server
clipboard; a further authenticated public-path probe requires the entire
canonical raw frame1024x768x4. These probes never paste/resize and never recapture
or rewrite K. The frozen original probe remains byte-identical after UI input.

`runtime_reconnect_verified` describes the Runtime's independent fresh connection.
Actual browser2socket/2exchange/2canvas reconnect is separate PWA authority,
combined in the joint record; a denied probe never counts as browser reconnect.
The result also reports runtime full-frame/permission-denial checks, original K,
frozen-probe identity, output save and cleanup. It correlates actual
run/lease/runner/session/surface IDs with ready metadata. Passed requires every actual fact and all original lifecycle assertions. Facts
are recorded individually: observed true/false are retained after later failure;
unobserved values are null. The actual clipboard field retains the checkpoint
value, while post-disconnect value/observation are separate. In particular, K
and output-save observations are not erased by a later Surface failure;
cleanup is measured independently for OCI resources, public/hidden ports,
journal and lease directory, including after test panic. Unknown is never passed.

## Verification so far

- Mac Worker180 passed /18 existing ignored before the heartbeat follow-up;
  final focused fixture5 tests passed; all-target
  Worker/netd Clippy and rustfmt passed.
- Linux Worker/netd330 passed /23 existing ignored before the heartbeat follow-up;
  final focused fixture5 tests and
  all-target Clippy passed. The source includes #1541's pinned noVNC packet
  negotiation regression. Same-source process tests explicitly use its Worker
  binary through existing ATO_TEST_WORKER_BIN.
- Independent read-only review verified all3 changed test files, no remaining
  P1/P2 after clarifying reconnect scope, adding false-permission/full-frame
  probes and extending failure cleanup measurement.
- Real invalid-done acceptance: original root Native Run reached ready, rejected
  abort record, stopped/saved output, returned expected nonzero test exit and
  result.status=failed with cleanup_confirmed=true and
  oci_resources_cleanup_confirmed=true. It did not report application success.
- Initial Chrome joint attempt failed before a canvas appeared; it is not
  acceptance. The orchestrator's upstream URL omitted the actual Surface path;
  that forwarding/diagnostic work belongs to the browser peer.
- A separate >10-second delayed-abort red run proved the fake control plane
  lacked the existing10-second heartbeat endpoint, panicked and left the real
  Runner safely bounded to90 seconds. The test-only server now validates POST,
  exact Runner path, recovered/active/max slot facts and basic collection/host
  shapes, then ACKs200{}. This is fixture shape validation, not a replacement
  for production API authorization/schema. No production timer was changed.
- Green actual: after ready12 seconds then abort, one real heartbeat was ACKed,
  the rejected done still produced statusfailed, output saved and full cleanup
  confirmed. Done03:18:32.711→result03:18:34.604 UTC:1.893 seconds. The result
  includes heartbeats_acked. Both red/green logs are retained.
- Second Chrome joint attempt reached actual initial640 frame, positive14-byte
  Latin-1 paste, resize1024x768, second WebSocket/frame reconnect and Stop with
  zero browser errors/network violations. Runtime's former clipboard check ran
  after disconnect and observed None, so the overall attempt correctly failed
  with cleanup confirmed. This did not prove clipboard arrival or persistence.
  The new checkpoint measures actual arrival while connected; post-disconnect
  None is a separate observation, not an assumed lifetime guarantee.
- Checkpoint follow-up: Mac and Linux7 focused fixture tests passed, with
  Worker/netd all-target Clippy and rustfmt. Strict checkpoint/done, missing
  checkpoint, private files, partial facts and all-facts AND are covered.
- Negative real acceptance: fresh root `/opt/ato/.tmp/native-browser.Ux3YXP`,
  ready then12 seconds and a valid checkpoint request without any paste.
  Actual Xselection None was rejected; checkpoint ACK reported failed/false,
  final statusfailed retained original_contract_satisfied=true/output_saved=true,
  unobserved facts null, one heartbeat ACK, full cleanup=true. All identity
  fields matched ready/ACK/result. No positive browser Run was started for this
  negative check.
- Third Chrome joint attempt passed on the frozen checkpoint candidate. Root
  orchestrated one checkpoint and one done transfer for fresh
  `/opt/ato/.tmp/native-browser.fVj3N0`, Runner946707 and
  lease `native-dispatch-946707-77b6f5242caebcc4`. Actual checkpoint Xselection
  was `636166e9090a` (Some expected), paste-denial close/unchanged was true,
  and the correlated12-field ACK passed before resize. After browser Stop,
  post-disconnect selection was independently observed as None. This supports
  the timing hypothesis in this test, not a clipboard-persistence guarantee.
- Final Runtime facts all passed: actual1024x768, false-permission resize,
  independent Runtime reconnect and canonical full frame, original K,
  byte-identical frozen GET, output saved, OCI resources/public+hidden
  listeners/journal/lease cleanup. A separate Docker query for this exact
  Runner found zero containers and zero networks. All658 source input hashes
  and the test-binary hash still matched after the run.
- Browser authority is separate: actual installed Chrome/shared
  SessionSurfaceRenderer/noVNC performed initial640x480 frame, user-gesture
  Latin-1 paste, checkpoint ACK, resize1024x768, second authenticated WebSocket
  and canvas frame on the same Surface, then Stop. Playwright passed8.6s;
  Runtime preparation6902ms, lease execution11363ms, ignored test19.03s.
  Root's21.6s orchestration includes SSH/preparation/browser/file synchronization
  and is not a product latency metric. No Hosted authentication or staging
  browser session was used. Control-plane/grants remain fixtures.
- Local evidence is under `.tmp/linux-evidence/joint3/` (ready/checkpoint/done,
  ACK/result, Runtime/browser logs, orchestration, independent cleanup and
  all-input hash check). Parent and frontend retain their independent logs at
  `.tmp/nbj-th4h7f3q` and the PWA fixture's matching artifact directory.
  Initial failed attempts and negative checks remain retained.

Own Linux source:
`/home/ekohsuke/ato-run/.tmp/native-browser-joint-20261024/source`.
All658 Rust/Cargo/literal-include input hashes were refreshed on source switch
and verified before/after: manifestSHA256
`4c9360e4c2537f4c5aa40082b204a44df5999322eb49f55ba6a44b47ba30416c`.
The same-source test executable is
`/home/ekohsuke/ato-run/.tmp/native-surface-20261023/runner-source/target/debug/deps/ato_connected_realization_worker-377b8bdb5c0f0403`.
Checkpoint candidate test-binary SHA256 is
`1364ab79c7d39be88d8f90b61aa5981890436f9d5b02b76a823dae131be3fa4a`;
launcher SHA256 remains
`88f43ef66033de3f3be0326f5eb37e8a11d11a029503fc2f7a0ba1391cc6d459`.
Shared target ownership was explicitly transferred; no shared Runner was installed
or enabled. Fixture root/temporary Docker config/ATO_HOME/TMPDIR are fresh;
HOME/home/CODEX_HOME and parent permissions are untouched. Source logs and hashes
remain in its own `.tmp/`, copied locally to `.tmp/linux-evidence/`.
