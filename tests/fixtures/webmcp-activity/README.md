# WebMCP Activity fixture

Deterministic Browser surface used by local and staging Actor/BYOA acceptance.
It offers `get_counter`, `increment_counter`, `slow_increment`, `set_label`,
and `navigate_phase` through the fixture producer bridge. One raw description
and one raw tool output intentionally contain an instruction-injection canary;
the Browser Adapter must never promote either value to an operation's safe
description or to MCP server instructions.

Run locally from the repository root:

```bash
python3 tests/fixtures/webmcp-activity/server.py --port 4179
```

The directory is also a normal Browser Capsule fixture. Copy it to a fresh
test directory before authoring so repository tests stay immutable, then use
`ato init`, `ato stop`, and `ato encap` in that copy. Hosted acceptance must
still run the resulting Capsule through the connected realization worker; the
local HTTP command alone is not a hosted-Activity acceptance path.
