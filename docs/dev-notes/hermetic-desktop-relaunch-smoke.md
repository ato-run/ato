# Hermetic Desktop relaunch smoke

This note describes how to drive a fully local, no-network smoke of the
installed-app relaunch flow using `ato install --from-local` (issue #561). The
goal is a deterministic regression for: install a fixture → start ato-desktop →
launch the installed app by its install profile key / capsule handle → verify it
reaches Ready in a WebView → close → relaunch.

`--from-local` builds a `.capsule` from a local capsule directory with **no**
registry / GitHub / network access and installs it into the current `ATO_HOME`
reusing the normal install + installed-state-ledger pipeline. The resulting app
relaunches through the same installed relaunch path as any registry/GitHub
install. It is a regression harness, not a general package importer.

## 0. Build the CLI and Desktop

```bash
cargo build -p ato-cli --bin ato
# Desktop is built/run from its own crate (see crates/ato-desktop/CLAUDE.md).
```

## 1. Create a hermetic ATO_HOME

Use a throwaway home so nothing touches your real `~/.ato`:

```bash
export ATO_HOME="$(mktemp -d /tmp/ato-from-local.XXXXXX)"
export ATO_TELEMETRY=0
# Optional belt-and-suspenders: point remote URLs at an unroutable address so any
# accidental fetch fails fast instead of reaching the real Store/GitHub.
export ATO_STORE_API_URL="http://127.0.0.1:1"
export ATO_GITHUB_API_BASE_URL="http://127.0.0.1:1"
```

## 2. Install the fixture from local

```bash
./target/debug/ato install \
  --from-local tests/fixtures/local-install/basic-web \
  --no-project --json
```

The JSON output carries the installed identity. Capture the install profile key:

```bash
IPK="$(./target/debug/ato install \
  --from-local tests/fixtures/local-install/basic-web \
  --no-project --json \
  | python3 -c 'import sys,json; \
docs=[l for l in sys.stdin if l.strip().startswith("{")]; \
import json; \
print(next(json.loads(l)["install_lifecycle"]["install_profile_key"] \
  for l in reversed(docs) if "install_lifecycle" in json.loads(l)))')"
echo "IPK=$IPK"     # ipk_<32 hex>
```

Sanity-check the hermetic store and installed-state DB:

```bash
find "$ATO_HOME" -maxdepth 4 -type f | sort | head
# expect:
#   $ATO_HOME/instances/instances/app_*/app.json
#   $ATO_HOME/instances/revisions/rev_*/artifact_manifest.json
#   $ATO_HOME/state/installed_state.sqlite3
#   $ATO_HOME/store/local/basic-web/0.1.0/basic-web-0.1.0.capsule
```

## 3. Start ato-desktop

Start the Desktop GPUI host bound to the same `ATO_HOME` (see
`crates/ato-desktop/CLAUDE.md` for the canonical run command). Keep the
`ATO_HOME`/`ATO_TELEMETRY` env exported above so Desktop and the CLI share the
hermetic state.

## 4. Launch by ipk / handle

CLI relaunch (bridges into the installed relaunch path):

```bash
./target/debug/ato launch "$IPK"
# or by capsule handle:
./target/debug/ato launch "capsule://local/basic-web"
```

Desktop relaunch (via the ato-desktop MCP bridge — see
`.claude/skills/ato-desktop-mcp-flow/SKILL.md`):

```json
{ "action": "NavigateToUrl", "url": "ato://app/<install_profile_key>" }
```

## 5. Verify Ready / WebView / close / relaunch

Expected visible page text from the `basic-web` fixture:

```text
Ato local-install basic-web fixture
```

Smoke checklist:

1. The guest WebView reaches Ready and renders the marker text.
2. Closing the window stops the session cleanly.
3. Re-issuing the launch (`ato launch "$IPK"` or `ato://app/<ipk>`) relaunches
   the same installed revision (pinned `current_revision`), not a fresh install.

## Launch-condition fixture (deferred / heavy)

`tests/fixtures/local-install/launch-conditions` declares an explicit-attach
`[state.data]` binding plus a fixed port so the installed-state ledger carries
`state` (`UserGrantRequired`) and `port` claims for driving:

```bash
ato launch "capsule://local/launch-conditions?state.data=prompt&port.main=18891"
```

The manifest schema only permits `state_bindings` on OCI targets, so this
fixture is OCI (`busybox`). Ato performs **no** network pull; the image must be
present locally before packing/launching it. The install ledger itself is read
from the manifest and does not need the image, but a fully hermetic CI install of
this fixture is not yet wired (it needs a pre-seeded image). Follow-ups:

- wire the launch-conditions fixture into a hermetic harness once a local OCI
  image seed is available (or a non-OCI state-binding path lands),
- add MCP smoke wiring + Desktop screenshot validation for the relaunch flow.
```
