# Native text paste boundary — 2026-10-24

> Status: historical evidence / past plan. Language: en.
> Preserved content; relative navigation updated. Current [documentation map](../../../README.md).

## Scope

Base: `876cb714085e17dc6156c5762ea1752f9aa8cef1`, frozen as
`integration/native-clipboard-base-876cb`. Branch: `feat/native-text-paste-boundary`.
Code/test/RFC commit: `d796d29159dfb9997082d053daf0beb7d4020a36`.

Only the explicit `start_text_paste_pixel_gateway` listener validates and admits
receive-only text paste. It additionally requires the existing signed optional
`clipboard_paste: true`, a User principal and exact execution scope. Permission
expires at the signed deadline. Existing HMAC, exact Origin, session/surface,
Run/lease/Runner, jti consumption and bounded TTL checks remain authoritative.
There is no new Core value, authentication scheme, table, D/K field or identity.

The legacy entry remains byte-transparent, including existing handshake and
unknown-extension behavior. Its unchanged producer denies CutText in both
directions. This PR does **not** enable that producer, advertise a Runner feature,
change Surface descriptors from clipboard=false, or connect API/PWA controls.

The new filter withholds each complete client message until validated. Latin-1
standard ClientCutText is bounded to 64 KiB; negative/extended clipboard, unknown
message types, control text, invalid padding and expired/denied permission refuse
before any offending message prefix is forwarded. C0/DEL/C1 controls are denied
except TAB/LF/CR; bytes U+00A0..U+00FF retain their RFC 6143 Latin-1 meaning. Empty
text may clear the isolated clipboard. Unsupported Unicode is a follow-up, not
UTF-8 encoded into legacy CutText. Accepted paste counts as user activity.

One paste per second is enforced by gateway-shared state, including different
connections/fresh assertions. RFB message assembly is bounded to 5 seconds and
64 KiB plus framing. A metadata-only socket observer independently bounds
WebSocket HTTP upgrade/frame/logical-message assembly to 5 seconds and 1 MiB;
fragmented data plus Ping cannot extend the deadline. It observes upgrade
read-ahead too. Idle between messages has no such deadline. The private write
has a 2-second bound only on this opt-in lane.

## Verification

- Baseline red: the added IPC scope regression fails on the base because an
  unknown optional paste claim is ignored, including a legacy unscoped claim.
  This is a regression for the newly introduced permission, not a claim that
  legacy producers previously allowed clipboard. The new scope checks turn it
  green and preserve omitted-field serialized bytes.
- Mac and Linux: `cargo test -p netd -p ato-ipc --offline --locked`, 249 passed.
  Mac has one existing ignored contract test; Linux additionally lists the new
  explicit real-container acceptance as ignored in the ordinary suite.
- The first final Mac full-suite invocation used a task TMPDIR whose derived
  Unix control-socket path exceeded macOS's socket-path limit (114 bytes for a
  representative child). The three egress daemon-start checks failed. Re-running
  with a fresh short workspace `.tmp/cp.*` root resolves the harness constraint;
  no source changes or test skips were used.
- Mac and Linux: `cargo clippy -p netd -p ato-ipc --all-targets --offline --locked
  -- -D warnings`, plus `cargo fmt --all -- --check` and `git diff --check` pass.
- Focused boundaries include all CutText split positions, denied/expired zero
  prefix delivery, signed User/execution checks, Latin-1 preservation, maximum
  size, negative/extended/padding/control/unknown refusal, absolute fragment
  deadlines, gateway-shared rate state and existing legacy byte relay/replay.
- Raw TCP acceptance performs the HTTP upgrade, starts a non-final masked WS
  Binary fragment, sends four interleaved Pings and observes closure at the
  fixed deadline with zero upstream RFB bytes. The observer also has a coalesced
  HTTP/header/fragment timing regression. Existing tungstenite rejects upgrade
  junk; timing does not depend on that behavior remaining unchanged.
- Independent read-only review found two initial defects (per-connection rate
  reset and a WS-fragment wait before the RFB filter). Both were fixed and
  reviewed again; no remaining P1/P2 was reported. Eight production-file review
  manifest: `2d06b1fa352ef232d5f6775f6f9b4d6b0505cb6fd4fedb86e859634220873bdd`.

## CPU Linux actual TigerVNC

The ignored test is
`services/netd/tests/pixel_text_paste_linux.rs::isolated_tigervnc_receives_only_complete_signed_latin1_paste`.
It uses the already-present image:

`ato-native-display-test@sha256:7076a8aadff1e92a09ba843928380aecfb00395cde9bce697e926174af04a9bd`

No image build, pull, package install, production helper change or feature
activation occurred. The fixture overrides the test container entrypoint with
Xtigervnc, receive CutText=1/send CutText=0, TCP RFB and TCP X11 disabled. It runs
with network=none, readonly root, all capabilities dropped, no-new-privileges,
128 MiB/0.5 CPU/32 PIDs, and the test caller's numeric UID/GID. Only fresh owned
0700 X11/RFB socket directories are bound; no host DISPLAY/socket/clipboard or
parent directory permissions are used. A temporary private loopback relay
connects the existing gateway TCP API to that owned RFB Unix socket.

The test uses the production HMAC authorizer and new opt-in gateway. A minimal
X11 selection client connects only to the fixture's Unix socket, so no xclip,
xsel, GUI client or package installation is needed. It observes:

- no clipboard value after a signed default-deny attempt;
- no clipboard value while the permitted packet's last byte is withheld;
- exact Latin-1 content after completion, including non-ASCII bytes;
- no server clipboard message, gateway listener closed after stop, and the
  exact created container absent after removal.

Final elapsed time: **174 ms**, measured from the post-Docker-start socket wait
through protocol assertions and gateway stop (Docker create/start and final
container removal excluded). Total test time including those operations:
**0.78 s**. An earlier same-protocol exploratory execution was 250 ms; the
174 ms result is from the final source. No Contract K receipt is claimed.

Linux source mirror:
`/home/ekohsuke/ato-run/.tmp/clipboard-boundary-20261024/source`.
All 572 Rust/Cargo/config inputs were checked before and after the execution and
refreshed before the shared-target build. Manifest SHA-256:
`7a25bef9fa99f17bd0ab3564584980aa9cbde599f4352dbbb23b8a69eae0f409`.

The exact same-source integration test executable was:
`/home/ekohsuke/ato-run/.tmp/native-surface-20261023/runner-source/target/debug/deps/pixel_text_paste_linux-df5ba2d40a2782c3`.
No Worker executable was used by this netd-only test. Jobs=2, dev debug=0,
incremental=0, offline/locked and FD limit 8192 were used. Each command used a
fresh task ATO_HOME/TMPDIR/exploration root and an empty task Docker config.
HOME/CODEX_HOME were not reassigned. Commands used an empty environment plus
explicit compiler paths for builds and standard system paths for the test.

Reproduction after building that target:

```sh
ATO_TEXT_PASTE_TEST_IMAGE='<existing exact pinned image>' \
ATO_EXPLORATION_TEST_ROOT='<fresh owned task root>' \
ATO_HOME='<fresh task ato root>' TMPDIR='<fresh task tmp root>' \
DOCKER_CONFIG='<fresh empty task docker config>' \
<same-source-test-executable> \
  --exact isolated_tigervnc_receives_only_complete_signed_latin1_paste \
  --ignored --nocapture --test-threads=1
```

This proves actual TigerVNC/netd/Unix-selection behavior with fixture signing
keys. It is not a Hosted API, authenticated browser, real Native Run, staging
rollout, clipboard UI or production operator-key acceptance. Existing app
exclusion from the private RFB mount was inspected in the unchanged Native
helper materialization code; this fixture creates no application container.

## Remaining integration

API/PWA permission minting and explicit user gestures, trusted helper receive-only
ABI, measured Runner capability admission, reconnecting permission refresh,
Unicode/extended clipboard, remote copy, GUI framebuffer resize, and staging
acceptance remain separate slices. `runtime_feature=native_text_paste_v1` is only
a proposed contract name in the RFC, not an advertised capability in this PR.
No deployment, flags, migrations, bindings, secrets, paid GPU or production
changes were made. Shared Linux Cargo target ownership was explicitly released
to the framebuffer-resize agent after the final execution.
