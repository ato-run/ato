# Installed Relaunch Node Fixture

Deterministic `source/node` capsule fixture for installed-app relaunch smoke.

The fixture intentionally uses only Node's built-in `http` module:

- no external npm dependencies
- no database
- no secrets
- no external network service
- fixed manifest port with `PORT` override support

## Manual install

Use a clean Ato home under the repository scratch directory:

```bash
mkdir -p .tmp/installed-relaunch-manual
export ATO_HOME="$PWD/.tmp/installed-relaunch-manual/ato-home"
export HOME="$PWD/.tmp/installed-relaunch-manual/home"
export ATO_TELEMETRY=0
mkdir -p "$ATO_HOME" "$HOME"
```

Install through the GitHub-source path using a local mock Store/GitHub server,
or use the integration test harness:

```bash
cargo test -p ato-cli --test installed_relaunch_fixture_install -- --nocapture --test-threads=1
```

The install JSON must contain:

```text
install_lifecycle.install_profile_key = ipk_<32hex>
```

Use that key for Desktop MCP relaunch:

```json
{
  "action": "NavigateToUrl",
  "url": "ato://app/<install_profile_key>"
}
```

Expected visible page text:

```text
Ato installed relaunch fixture
```
