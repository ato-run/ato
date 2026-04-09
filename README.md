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
cargo run --bin ato-desktop
```

Run tests:

```bash
cargo test
```

Build a macOS app bundle for artifact-import packaging:

```bash
cargo run --manifest-path xtask/Cargo.toml -- bundle --target darwin-arm64
```

Publish a source-derived desktop capsule locally:

```bash
cargo run --manifest-path ../ato-cli/Cargo.toml -- publish --dry-run
```

Install the published desktop capsule on another macOS arm64 device:

```bash
ato install <publisher>/ato-desktop --project
```

Bundle output:

```text
dist/darwin-arm64/Ato Desktop.app
```

The bundle command stages:

- `Contents/MacOS/ato-desktop`
- `Contents/Helpers/ato`
- `Contents/Resources/assets`

Runtime helper resolution order:

1. `ATO_DESKTOP_ATO_BIN`
2. bundled `Contents/Helpers/ato`
3. `ato` on `PATH`

Runtime asset resolution order:

1. `ATO_DESKTOP_ASSETS_DIR`
2. `./assets` from the current working directory
3. bundled `Contents/Resources/assets`
