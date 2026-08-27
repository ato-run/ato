# Connect an external coding agent to an Activity Actor

`ato-activity-mcp` is a pure stdio MCP server for one ato.run Activity Actor.
It keeps the Actor and Actor Run stable while temporary Codex, Claude Code, or
other MCP Controller sessions reconnect. It does not add Activity or Actor to
the Ato Semantic Core.

Download the one-time connection file from the Activity page and protect it
before starting the server:

```console
chmod 600 ./actor-connection.json
```

The file contains `api_url`, `activity_id`, `actor_id`, and `controller_key`.
Never put `controller_key` in a command-line argument, URL, prompt, or checked-in
configuration. The server accepts the file through `--connection-file` or
`ATO_ACTIVITY_CONNECTION_FILE`; it rejects group/world-readable files on Unix.

For Codex, use project-scoped `.codex/config.toml` in a trusted project:

```toml
[mcp_servers.ato_activity]
command = "/absolute/path/to/ato-activity-mcp"
args = ["--connection-file", "/absolute/path/to/actor-connection.json"]
required = true
startup_timeout_sec = 15
tool_timeout_sec = 60
default_tools_approval_mode = "writes"
```

The server exposes exactly nine stable tools: context, Surface observation,
operation listing/invocation, Memo read/update, Interaction list/send, and
control release. WebMCP page tools remain untrusted `OperationDescriptor` data
returned by `list_operations`; they never become dynamic MCP tools or MCP
instructions. Call `observe_surface` before mutation and again after stale state
or human intervention, preserve durable handoff state in the Actor Memo, and
call `release_control` when finished.
