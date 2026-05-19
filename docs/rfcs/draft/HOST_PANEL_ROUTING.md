# Host Panel Routing

Status: Superseded

The original host-panel design embedded a React/Vite app inside a Wry WebView
and routed desktop-owned panels through a custom host protocol. That design has
been retired as part of the Focus View migration.

Current ato-desktop keeps `HostPanelRoute` only as desktop state for native
panes such as the launcher, settings, and capsule detail views. Rendering is
handled directly by GPUI panels, not by a bundled frontend app or a custom host
asset protocol.

Consequences:

- Desktop-owned panels must not require a frontend build artifact.
- CI and release jobs build ato-desktop as a Rust/GPUI application.
- WebView lifecycle code is reserved for guest capsules, external web routes,
  auth handoff, and terminal surfaces.
- Settings and capsule detail navigation is driven by Rust state/actions, not
  browser history events from an embedded host frontend.

Any future panel work should extend the GPUI Focus View surface instead of
reintroducing a parallel desktop frontend.
