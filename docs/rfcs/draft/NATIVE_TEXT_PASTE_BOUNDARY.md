# Native text paste boundary (draft)

This extends the physical Surface assertion and netd RFB gateway, not K/D, a
Core Protocol, an authentication model, or a database table. Existing signed
claims, descriptors and profiles retain their bytes when paste is absent.

## First slice

Optional `clipboard_paste: true` is signed using the existing Runner Surface
key. It requires a User principal and exact execution scope. Absence/false denies
paste. The resulting permission expires at the assertion expiry even on a
long-lived socket. Existing Origin, session, surface, execution, signature, jti
one-use and bounded TTL checks remain mandatory. Readiness emits no permission.

Only an explicit paste-capable listener uses the validating client stream
filter. Ordinary legacy listeners remain byte-transparent and must continue
using a producer with CutText disabled in both directions; legacy gateway bytes
are still passed unchanged and the producer enforces that denial. The new listener
buffers each complete RFB client message before forwarding it. It admits the
pinned RFB 3.8/None handshake and known bounded client messages, nonnegative-length
standard ClientCutText only, at most 64 KiB ISO-8859-1 (Latin-1) plain text, and no
NUL or control characters (C0, DEL, C1) except LF/CR/TAB. Negative/extended,
unknown, malformed, denied, expired
or incomplete timed-out messages fail closed before any message prefix reaches
RFB. Fragment assembly has a fixed 5-second deadline and bounded memory. A
maximum of one paste per second per Surface gateway, shared across connections,
bounds workload delivery and allocation churn.
Server-to-client clipboard stays disabled at the isolated trusted producer.

No production helper or Runner advertises or enables this feature in this PR.
An app cannot opt into the listener or select a host display. A later trusted
helper ABI must support receive-only clipboard inside the Run's isolated X
server, keep SendCutText=0, and leave the app without the private RFB socket mount.
Only after that ABI and gateway are joined and tested may a Runner advertise
`runtime_feature=native_text_paste_v1`. A mere decoder registration or signed
claim cannot establish physical support.

## API/PWA follow-up proposal

The API authorizes the existing private Native owner path and live accepted
Run/lease/Runner, requires the new measured runtime feature, then explicitly
mints the optional permission on the existing POST Surface access exchange. No
guest/public scope or automatic permission upgrade. Capability metadata stays
false until that complete path is available. User-gesture paste handles only
text/plain, with size checks, denial feedback and ephemeral browser state.
noVNC clipboardPasteFrom reuses the authenticated connection. The initial bridge
must use bounded standard CutText and not negotiate extended clipboard. It must
not log text, persist it in receipts/snapshots, read host DISPLAY, or automatically
synchronize navigator.clipboard. Remote copy and framebuffer resize are separate
slices. Browser user gestures are UI safeguards, not remotely attestable claims;
the server enforces owner permission, scope, direction, size, rate and expiry.

RFC 6143 §7.5.6 defines positive ClientCutText as Latin-1, not UTF-8.
Bytes U+00A0..U+00FF are preserved exactly; Unicode outside Latin-1 is unsupported
until a separately bounded extended clipboard implementation. PWA must refuse
unsupported characters before encoding, never silently truncate code points.
Reference: https://www.rfc-editor.org/rfc/rfc6143.html#section-7.5.6

The opt-in socket additionally bounds WebSocket header/frame/logical-message
assembly to 5 seconds from its first byte, including Upgrade read-ahead. Idle
between messages has no such deadline. Control Ping/Pong inside fragmented data
cannot reset it. The observer stores only framing metadata, never clipboard
text; tungstenite retains protocol validation and the 1 MiB message cap.
