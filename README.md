# ato-desktop

Phase 2 implementation of the Ato Desktop shell using GPUI and Wry.

Current scope:

- Single-window GPUI shell
- One active child WebView mounted through Wry
- Custom `capsule://` protocol for guest assets
- Preload bridge with fail-closed capability checks
- Workspace navigator, overview rail, and agent peek panel
- Pane close tears down the mounted guest session

Run locally:

```bash
cargo run
```

Run tests:

```bash
cargo test
```
