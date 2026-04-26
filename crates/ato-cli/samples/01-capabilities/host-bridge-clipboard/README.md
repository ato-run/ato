# host-bridge-clipboard

Demonstrates the ato **host bridge IPC** and **capability gate** using clipboard access.

## What this demonstrates

- `[capabilities] host_bridge = ["clipboard.read", "clipboard.write"]` — explicit capability declaration required
- `ATO_IPC_SOCKET` environment variable injected by ato-desktop into the capsule
- Unix socket JSON-RPC protocol for invoking host capabilities
- Denied response when capability is not granted in the consent UI
- Error behavior when bridge is absent (no `ATO_IPC_SOCKET`)

## Capability model

```toml
[capabilities]
host_bridge = ["clipboard.read", "clipboard.write"]
```

ato-desktop shows a consent dialog on first run. The user must explicitly grant
`clipboard.read` and `clipboard.write`. Without a grant, the bridge returns
`{ "status": "denied" }` and the capsule exits with an error.

## Run

Requires **ato-desktop** to be running. The desktop injects `ATO_IPC_SOCKET`
pointing to the Unix socket for this capsule session.

```bash
# Inside ato-desktop (injected automatically):
ato run .

# Outside ato-desktop (expected failure — bridge absent):
ato run .
# → ERROR: ATO_IPC_SOCKET not set — ato-desktop bridge is required.
```

## Expected behavior

| Context | Outcome |
|---------|---------|
| ato-desktop, permission granted | Clipboard read, timestamp appended, clipboard written |
| ato-desktop, permission denied | Exits with "DENIED: clipboard.read was not granted" |
| No ato-desktop (headless CI) | Exits with "ATO_IPC_SOCKET not set" error |

## IPC protocol

The capsule communicates with ato-desktop over a Unix domain socket at
`$ATO_IPC_SOCKET`. Each request is a newline-delimited JSON object:

```json
{
  "kind": "invoke",
  "request_id": 1,
  "command": "read",
  "capability": "clipboard.read",
  "payload": {}
}
```

The bridge responds with:

```json
{ "status": "ok", "request_id": 1, "message": "", "payload": { "text": "..." } }
```

or, if the capability was denied:

```json
{ "status": "denied", "request_id": 1, "message": "clipboard.read not granted" }
```

## Related

- `docs/upstream-issues/` — bridge API spec gaps being tracked upstream
- `AGENTS.md` (capsuled-dev) — bridge/IPC patterns reference
