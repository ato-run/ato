# CPU managed embedding transport acceptance — 2026-10-24

Branches: `feat/managed-embedding-private-relay` (Rust, child of #1524) and
`feat/managed-embedding-relay-authority` (API, child of #856). No staging/flag,
production/provider key or paid-provider changes were made.

Validated the actual production Broker router with a migrated Miniflare D1,
local CA-signed HTTPS leaf, and an outbound adapter accepting only the fixed
OpenRouter embedding request. All provider responses came from this stub.
An SSH loopback-only forward carried this private fixture to `ubuntu-sugamo`;
no public listener or host service was installed.

The ignored native Docker test was explicitly executed on that CPU host. An
unprivileged readonly Python container with 128 MiB/0.5 CPU/32 PID limits used
pinned official OpenAI 3.37.0 via the real Runner relay on its own internal
bridge. It returned 1536 values for both default base64 and explicit float.
Chat was refused. Direct 1.1.1.1:443 and host gateway ports 22/443 all failed.
Redirect, >128 KiB Broker response and a delayed Broker response were refused;
no workload cookie/custom header reached the Broker. After the test stopped
the real Run lease, its embedding call returned 403. Exactly two upstream stub
requests were made in the final fixture; zero paid provider requests.

The test confirmed relay listener release and explicit owned network deletion.
Existing host INPUT rules were checked with `iptables -C`; no new host service
or policy was installed. Only test-owned containers/networks were manipulated.

Other validation: local two-crate unit/regression 315 passed (three existing
Linux-only worker tests ignored on macOS), all-target Clippy, formatting and
whitespace checks. API issuer + existing Broker tests: 32 passed; direct tsc
passed. The SDK scalar predecessor's broader regression/base comparison is
recorded separately. Linux-specific Docker acceptance is distinct from complete
control-plane claim/dispatch or hosted staging acceptance; these remain pending.

Reproduction requires the companion API test harness:

1. Build `tests/managed-embedding-relay/Dockerfile` as
   `ato-managed-embedding-sdk:acceptance-20261024` on an isolated CPU Docker host.
2. Run API `tests/managed-embedding-sdk/run.mjs` with
   `ATO_SDK_FIXTURE_SERVE=1` and TMPDIR under that checkout's `.tmp`.
3. Carry only the emitted short-capability `fixture.json` and public `ca.pem`
   into the Rust checkout's owned `.tmp`; forward its HTTPS port loopback-only.
4. Set `ATO_MANAGED_SDK_FIXTURE_DIR` to that owned directory, then run
   `cargo test -p ato-connected-realization-worker --lib official_sdk_container_uses_only_owned_relay_and_releases_resources -- --ignored --nocapture`.
5. Stop the fixture/forward and remove only their owned capability files.

This does not install a new Runner, enable managed preview, prove production
provider behavior, or alter Contract acceptance. Existing group/ServiceEgress
lifecycle implements production cleanup; the actual Docker test exercises the
same owned network/relay and a bounded container directly.
