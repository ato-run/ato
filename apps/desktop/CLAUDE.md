# CLAUDE.md

Guidance for working in `apps/desktop`.

## Read this first

**The Ato Desktop product is not in this directory.** It is
[`ato-run/ato-desktop`](https://github.com/ato-run/ato-desktop): a Tauri shell,
with the Local Coordinator, the bundled `ato` Runner and the acceptance system.
Work on the Desktop product belongs there.

## What is here

A legacy launcher shim, three files:

| File | Responsibility |
|------|----------------|
| `src/lib.rs` | `dispatch()` — shells out to the `ato` CLI for each `ComputationCommand` variant; `launch_console()` opens the web console |
| `src/main.rs` | Arg parsing for `--version` and `--run <file.capsule>` |
| `src/bin/ato_desktop_mcp.rs` | MCP entry point |

Dependencies: `anyhow`, `ato-ipc`, `serde_json`. That is the whole graph.

```bash
cargo test    # from apps/desktop
```

## What this file used to claim

It described a GPUI + Wry shell with `app.rs`, `state/mod.rs`, `ui/`,
`webview.rs`, `bridge.rs` and `orchestrator.rs`, a `capsule<partitionId>://`
custom protocol, and a state → WebView sync model driven by
`DesktopShell::render`.

**None of those modules exist.** The GPUI shell was dismantled; nothing here
depends on `gpui` or `wry`. Treat the manifest, the call graph and the source as
authoritative — never this file or the README — when judging what is
implemented.

## Packaging

`xtask/` and `installer/` were migrated with history to
`ato-run/ato-desktop/packaging/`. The copies here still back
`desktop-release.yml`, which stages this shim.
