# Bounded RFB noVNC encoding compatibility

Date: 2026-10-24. Parent: Resize boundary #1540,
`90746ed9566d3640164e480668e4d838d4de5555`.
Code/tests/RFC: `2dd517711dfbb00be003dc764eedee4696cc9a96`.

Pinned noVNC 1.5.0 always includes ExtendedClipboard in SetEncodings. A server
capability reply may elicit negative ClientCutText before a gesture, and the
existing bounded gateway correctly refuses all negative CutText. Connecting
that client directly without suppressing the unsupported negotiation can
therefore disconnect it. This is a prerequisite for the separate controlled
Native UI/producer path, not permission to enable that path.

## Change

Only the common bounded client filter changes production behavior. After
validating the original complete message, it removes every four-byte encoding
`0xc0a1e5ce` from SetEncodings and recomputes the u16 count. Every other encoding
byte/order and header padding is preserved. Zero remaining encodings is valid.
The complete original length/deadline bounds apply before shrinking: an
oversized or incomplete original cannot become admissible by normalization.
Every later SetEncodings is treated identically.

The policy applies regardless of the physical mask or signed permissions, so
an ungranted client cannot trigger unsupported automatic negotiation. It grants
neither paste, copy nor resize. Negative ClientCutText still fails before a
prefix; positive Latin-1 text retains all existing authorization, expiry, size,
rate and physical-producer requirements. Other RFB message types/handshake
bytes, the legacy transparent gateway, IPC claims, public listener contract,
Native ready lifecycle, K/D/Run and Runner advertisements are unchanged.
There is no private noVNC production hook, library patch or per-app server.

## Shared actual pinned-client wire vector

The PWA review/fixture produced the packet using the actual pinned noVNC
`RFB.prototype._sendEncodings` with a test-only socket recorder and ordinary
settings: depth24 / quality6 / compression2. This private test context is not
used by production. Source inspected locally:
`node_modules/@novnc/novnc/lib/rfb.js` lines2203–2231 plus its pinned encodings.
The unchanged shared fixture is
`services/netd/tests/fixtures/novnc-1.5-encodings.json`.

- File SHA-256: `64a05d0f5928d6cff8b73f269cc42747d6c0a1bab49d35f30639d77ace0fdd6c`.
- Original: 92 bytes / 22 entries, ExtendedClipboard at entry19.
- Original packet SHA-256:
  `d7d659fe45a0cf72fee0ffabfcc988c0c52af221988431c2339e49d3e84fc01f`.
- Bounded output: 88 bytes / 21 entries, only that entry removed.
- Output packet SHA-256:
  `5737b9187b2562f591dcc563058cf3a16b8501479e19b73d34f9f43fb6f72744`.
- Legacy output remains exactly the original 92-byte packet.

The new Rust golden failed on the unmodified parent filter, forwarding all22
entries. The forwarding change makes it pass. This cross-language wire fixture
checks real pinned-client packet compatibility; it is not browser-to-live-GUI,
Hosted/staging, Contract acceptance or evidence of an enabled product control.

## Validation

Isolated macOS source/target `.tmp/fenc/.tmp`, offline/locked, jobs2/debug0 /
incremental0, short fresh workspace TMPDIR/ATO_HOME/test root:

```sh
cargo test --offline --locked -p netd --all-targets -- --test-threads=1
cargo clippy --offline --locked -p netd --all-targets -- -D warnings
cargo fmt --all -- --check
git diff --check
```

All netd targets: **149 passed / zero ignored**, including **121 library tests**
and eight added regressions. Clippy/fmt/diff checks passed. The original-parent
red log and final logs remain in `.tmp/fenc/.tmp/`.

Coverage includes duplicate/all/zero/nonmatching encodings, preserved preference
and adjacent messages, each fragment split, original-size limit including an
all-removed maximum message, oversize refusal before a prefix, the absolute
assembly deadline, every later message, four physical-mask configurations,
unchanged legacy socket bytes, incomplete-message zero-prefix forwarding and
negative CutText refusal after rewritten negotiation. Existing authorization,
clipboard, resize/rate/expiry, lower WS fragment and Terminal tests passed.

Independent read-only review found no new P1/P2. Production file
`pixel_text_paste.rs` SHA-256:
`93ebadc262e4aba17a5b57e52b9023c66d78654fc470d6716797450555721069`.
`pixel_gateway.rs` SHA-256:
`3f2318b514f6dafb72065af880c578f2d055debc0ba13236f2eb5213fa5547a9`;
its differences are test helpers/cases only.

## Remaining joint acceptance

No shared Linux Cargo target, Docker, real API, key, Runner install, deployment,
feature flag, provider inference or paid resource was used in this child.
The Linux target remains with the separate trusted producer/lifecycle owner.
The prior Resize/clipboard actual CPU measurements belong to #1540 and were
not repeated or relabeled as this child's acceptance.

After combining the trusted producer, lifecycle and PWA controls, a separate
headless browser/noVNC → controlled gateway → pinned helper fixture can verify
this negotiation without real credentials. It must distinguish test-only auth
proxy/keys from Hosted/manual/staging authorization and must measure actual
frames, text delivery, size change, refusal and cleanup before enabling any
capability. Extended Clipboard/remote copy remain unsupported; this change
suppresses their negotiation rather than implementing them.
