# Native framebuffer resize boundary (draft)

Status: Draft.

This extends the existing physical Surface assertion and the bounded RFB
listener. It does not change K, the initial D viewport, the frozen application
probe, the selected Capsule, or the Run. There is no host desktop selection.

The optional `framebuffer_resize` permission uses the existing signed Surface
claims. Absent/false retains old wire bytes and grants no permission. Like text
paste, it requires a User principal and exact Run/lease/Runner execution scope.
Readiness assertions grant neither operation. Existing signature, Origin,
session/surface, execution, jti replay and TTL validation remain authoritative.
The permission expires at its assertion deadline on an already-open socket.

`start_interactive_pixel_gateway(config, authorizer, PixelGatewayControls)`
requires both a trusted physical producer capability mask and the independently
signed per-connection permission. `PixelGatewayControls` defaults to no text
paste and no resize. The existing `start_text_paste_pixel_gateway` wrapper sets
text paste only, and rejects resize even if a signed resize claim is true.
There is one common bounded parser/listener implementation.

Only the explicitly bounded listener applies this boundary. Legacy transparent
listeners retain their existing behavior and do not become capable from a new
claim. No Runner capability or product resize control is enabled in this slice.

## Complete-message admission

The shared client-stream filter buffers an entire RFB SetDesktopSize message
before any of its bytes reach the isolated producer. This slice accepts exactly
one screen: a 24-byte message, zero header padding, zero screen origin, matching
header/screen dimensions, and zero screen flags. Dimensions must be positive,
width at most 4096 and height at most 2160. The screen identifier is opaque to
the gateway and refers only to the already-bound isolated RFB server. It does
not bind another display, process, host or resource.

Malformed, multi-screen, oversized, unauthorized, expired and incomplete
messages fail closed. The existing fixed 5-second fragment assembly deadline
never extends when more fragments arrive; permission is rechecked at message
completion. An accepted resize counts as input activity. One atomic limiter
belongs to the gateway and admits at most one resize per second across
concurrent connections and reconnects. A malformed/denied command does not
consume that allowance. Limits bound allocation churn; they do not imply the
server accepted a resize or prove application behavior.

## Verification and follow-ups

Actual acceptance must use the owned isolated TigerVNC helper with Unix-only
RFB, no host DISPLAY, no workload RFB mount and no host desktop. Evidence must
include the ExtendedDesktopSize success reply, actual frame dimensions and
bytes after resize, retained dimensions on a new connection, denied-message
zero upstream bytes, and owned cleanup. A Surface ACK is separate from the
original application Contract evidence; resizing cannot rewrite or replay K.

API permission minting, capability advertisement and PWA controls require a
later coordinated change. They must keep the existing owner/accepted Run,
lease/Runner and transport checks. Physical support must be measured before
advertisement; a claim, decoder registration or UI flag alone is insufficient.
