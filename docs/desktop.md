# Desktop

`ato-desktop` remains a separate release/build boundary. It does not link the
kernel, semantics, adapters, or providers. The native launcher opens the web
console and delegates computation commands to the `ato` process through DTOs
owned by `ato-ipc`.

```bash
ato-desktop
ato-desktop --run .
```

`ato-desktop-mcp` exposes `run_computation` and `list_runs` over MCP stdio and
uses the same `ato-ipc::computation` request types.
