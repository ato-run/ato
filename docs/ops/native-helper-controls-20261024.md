# Trusted Native helper controls — 2026-10-24

## Scope and immutable boundaries

Child of Native framebuffer boundary PR #1540, code base `90746ed9566d3640164e480668e4d838d4de5555`.
Existing common TigerVNC helper and Native owned helper/app lifecycle are reused.
No Kernel/Core, K, D, descriptor, RuntimeExecutionEvidence, receipt schema, table,
authentication system or application-specific display server is added.

Operator controls `--native-pixel-text-paste` / `ATO_NATIVE_PIXEL_TEXT_PASTE`
and `--native-pixel-framebuffer-resize` / `ATO_NATIVE_PIXEL_FRAMEBUFFER_RESIZE`
default false. Flags require enabled Native Pixel and a verified, warm, admitted
helper with exact image-config label `org.ato.native-display.controls-abi=1`.
The existing OCI validator authenticates all digest/closure bytes before a
64KiB bounded read of its exact config. Legacy callers skip that optional reader.
A control flag on legacy/malformed helper refuses before container creation.

The helper retains its old two-argument ABI. The optional fixed ABI is
`width height --controls-v1 <0|1> <0|1>`. Only trusted Runner configuration emits
these arguments. `SendCutText=0` and Unix-only RFB/X11 boundaries remain fixed.
The app receives only its own X11 socket and Xauthority, never the private RFB
mount, host DISPLAY, host clipboard, Docker socket or Runner credential.
Physical helper/gateway masks must match before creation; a ready controlled
gateway still requires signed User + execution + expiry + Origin + replay gates.
Terminal rejects GUI controls. Default/legacy listeners and ready JSON omit
new support information.

The top-level optional ready field is `surface_controls` with boolean
`clipboard_paste` and `framebuffer_resize`. It comes from the actually ready
owned Run, outside D/descriptor/execution evidence and receipt. Heartbeat features
`runtime_feature=native_text_paste_v1` and
`runtime_feature=native_framebuffer_resize_v1` require available verified helper
admission, not just flags. API receiver #876 and browser permission/UI are separate
changes; no deployed Runner flags were changed.

## Local verification

- Baseline new-flag parsing regression failed before implementation (unknown flag).
- Mac IPC/netd/Worker all targets: 440 passed; 18 existing ignored integration cases.
- Worker final targeted repeat: 176 passed / 17 ignored. IPC/netd unrelated targets
  were not repeated without a new production change.
- Mac all-target Clippy on IPC/netd/Worker, rustfmt, shell syntax, diff whitespace passed.
- Optional marker exactness/absence/unknown, full digest/closure before marker,
  corrupt layer/wrong image, bounded config, fixed argv, default flags/disabled
  Native, Terminal refusal, helper-mask mismatch-before-action are covered.
- Controlled and legacy first-frame readiness use one shared cleanup lifecycle;
  unauthorized probe stops the listener, actual-ready idle monitor stops it,
  repeated stop is safe. Ready support and receipt identity remain separate.
- Independent read-only security review found a non-Unix `Option<()>` initializer
  type reference; cfg Unix/non-Unix separation fixed it. Re-review: no remaining
  P1/P2. Non-Unix cross-compilation was not executed.

## CPU Linux acceptance

Linux IPC/netd/Worker all targets: 441 passed / 23 existing ignored. All-target
Clippy passed. The initial Process state tests used the test executable as shim
and three exited 101; building the same-source Worker and setting the existing
`ATO_TEST_WORKER_BIN` made all three and the full set pass without source/skip or
budget changes. Both logs are retained.

Actual tests passed on the same-source Worker test executable:
`/home/ekohsuke/ato-run/.tmp/native-surface-20261023/runner-source/target/debug/deps/ato_connected_realization_worker-377b8bdb5c0f0403`.
The separate same-source process shim is target/debug/ato-connected-realization-worker.

- Non-root direct Native Run with controls: denied paste leaves clipboard empty;
  fragmented paste leaves no prefix; complete Latin-1 bytes reach only its own
  authenticated X11 selection. Real resize changes its display from 640x480 to
  800x600 and reconnect observes that size. No server clipboard is received.
  Fixed helper argv and no app RFB mount are inspected on actual containers.
  Original K and D refs, frozen probe bytes and fully_satisfied=true remain the
  same; both containers and endpoints are reclaimed.
- New-helper legacy two-argument/default-off actual Run also passed: 2767ms
  warm readiness, no reported controls, original K and owned cleanup intact.
- Final direct Run warm readiness: 6134ms, separately 6969ms verified image input;
  preceding successful samples were 3075ms and 3845ms. These are CPU debug
  measurements, not a staging latency SLA.
- Fresh root ConnectedWorker::new with HostBoundary UID/GID65534: startup
  preparation 6835ms, lease 4737ms; real signed controls, top-level ready support,
  no support extension in execution/receipt, original K, read-only asset input,
  stop output save and all owned resources/credentials clean. Credential canary
  remains root-owned0600 and excluded from workload.

Test control-plane/grants are local fixtures; Docker/X11/RFB/gateway/owned
cleanup are real. This is not Hosted API, R2, PWA/browser, staging or production.
The first root fixture incorrectly performed interaction inside ready ACK and
hit the unchanged 500ms report budget. The test now performs it only after ACK;
production deadline and refusal behavior are unchanged. A subsequent attempt
failed startup and correctly did not verify K; its fixture expected only result
output and obscured the start-failure artifact. Its log is retained, diagnostic
reporting was added, and the next unchanged-budget run passed. The exact cause
of that one startup failure was not established; it is not classified as a
proven pre-existing failure or hidden as successful.

Own source mirror: `/home/ekohsuke/ato-run/.tmp/native-controls-helper-20261024/source`.
Shared target was explicitly handed off from the preceding Resize task. All
595 Rust/Cargo/config/compile-input hashes are checked before/after, and all
inputs had mtimes refreshed when switching to this source. Final input manifest
SHA256 is `6e571b9101c6e646800ea53b8634988b22241e7f86c88c1e36c9c192bce63609`.
Logs and before/after hash checks remain under this own source's `.tmp/` and
are copied to the worktree `.tmp/linux-evidence/`.

The CPU fixture helper is an offline COPY-only derivative of already-present
config ID `sha256:7076a8aadff1e92a09ba843928380aecfb00395cde9bce697e926174af04a9bd`.
New immutable manifest: `ato.local/native-helper-controls@sha256:7f667967b869f1bb6da078c54f9793c7ef8313765fdb6257283cf0661b543744`.
No packages/pulls/registry publication/host services were installed.
The first BuildKit attempt tried registry metadata resolution for the local
config-digest reference and was refused; no image was pulled. A local-ID legacy
builder then succeeded with COPY-only and network disabled. Its unsupported
COPY chmod option was removed after setting mode on the own context file.
All initial/final build logs are retained.

Fresh task-specific ATO_HOME/TMPDIR/empty Docker config roots are used; HOME,
home and CODEX_HOME are untouched. Root acceptance uses an own marker-bearing
folder under existing `/opt/ato/.tmp`, existing HostBoundary numeric UID/GID,
private UDS and credential-canary checks; no parent permissions are changed.

## Remaining conditions

All controls remain off in shared Runners. Pinned noVNC 1.5 always advertises
ExtendedClipboard and automatically replies with forbidden negative CutText;
its safe negotiation filter is a separate child of #1540. This PR never weakens
negative/extended refusal. UI uses public noVNC APIs after that child; actual
noVNC/browser acceptance is separate. Clipboard supports receive-only Latin-1,
not Unicode/extended clipboard, remote copy or a host desktop. Real Hosted/R2,
staging owner session, production rollout and GPU are outside this CPU fixture.
