# ato-desktop (legacy launcher shim)

> **This is not the Ato Desktop product.** The Desktop product — the Tauri
> shell, the Local Coordinator, the bundled Runner and the acceptance system —
> lives in **[`ato-run/ato-desktop`](https://github.com/ato-run/ato-desktop)**.

## What this crate actually is

Three files. `src/lib.rs` shells out to the `ato` CLI for the five
`ComputationCommand` variants; `src/main.rs` parses `--version` and `--run`;
`src/bin/ato_desktop_mcp.rs` is a small MCP entry point. Its entire dependency
list is `anyhow`, `ato-ipc` and `serde_json`.

```bash
cargo test    # from apps/desktop
```

## What it is not, any more

This README used to describe a GPUI + Wry shell: a single-window GPUI host, a
child WebView mounted through Wry, a `capsule://` protocol for guest assets, a
preload bridge with fail-closed capability checks, a workspace navigator,
overview rail and agent peek panel, and an `ato://` deep-link table with a CLI
mode.

**None of that exists in this crate.** The GPUI shell was dismantled, and there
are no `gpui` or `wry` dependencies here. The old text described modules
(`app.rs`, `ui/`, `webview.rs`, `bridge.rs`, `orchestrator.rs`) that are not in
`src/`. It was left in place long enough to be mistaken for a porting target,
which is the specific harm this rewrite exists to prevent: **read the Cargo
manifest and the source, not this file, to decide what is implemented.**

## Packaging

`xtask/` and `installer/` were migrated with history to
`ato-run/ato-desktop/packaging/`, where `xtask` is reduced to the macOS signing
primitives the Tauri bundler does not cover. The copies here still back
`.github/workflows/desktop-release.yml`, which stages *this* shim rather than a
real product shell. Retiring that pipeline is deliberately **not** part of this
documentation cleanup — it needs a decision about whether this repository should
publish a desktop bundle at all.
