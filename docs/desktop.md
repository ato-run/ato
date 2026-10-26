# Desktop

Desktop is a separate optional build/release boundary under `apps/desktop`,
excluded from the root Cargo workspace. The current product entry is the Web
service ato.run. Desktop does not define a different Capsule identity.

Use the component's [README](../apps/desktop/README.md),
[Cargo manifest](../apps/desktop/Cargo.toml) and executable help for its current
launcher/MCP interface. Its IPC boundary is owned by `ato-ipc`. Old GPUI/Wry,
Tauri and multi-webview plans describe different historical implementations;
do not infer the current toolkit or commands from those plans.

Start with the [architecture map](architecture/README.md) and
[contribution guide](../CONTRIBUTING.md) before changing this boundary.
