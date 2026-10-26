# Getting started

Start in a checkout of this repository at a reviewed commit. Install the Rust
toolchain declared in [rust-toolchain.toml](../../rust-toolchain.toml). These
commands build and use that checkout's CLI; deployment of matching services is
not implied.

```sh
cargo build -p ato-cli --bin ato --locked
mkdir -p .tmp/getting-started
target/debug/ato pack samples/interop-static-k \
  -o .tmp/getting-started/static.capsule
target/debug/ato run .tmp/getting-started/static.capsule --no-open \
  --verification-receipt .tmp/getting-started/receipt.json
```

The [sample manifest](../../samples/interop-static-k/capsule.toml) declares an
immutable workspace, static serving, an HTTP body digest and captured source
identity. `pack` binds its authoring input into a portable application bundle.
`run` validates the bundle, realizes the declared route and verifies the HTTP
observations. `--no-open` verifies and exits without opening a browser.

Inspect `receipt.json`: it should name the bundle transport hash, K, D, target
and each observation, with `fully_satisfied = true` on success. Preserve failure
output and investigate the reported boundary; do not replace K with the
observed response to obtain a pass. Commands above were checked against source
during documentation maintenance; this PR does not claim a new runtime smoke.

For an application requiring Python, OCI, GPU, state or external Bindings,
read [Formation](formation.md) and [Runner / Hosting](runner-hosting.md) before
execution. Host capability and permissions are separate admission checks.

For the computation-root lifecycle, continue with [Local lifecycle](../run.md)
and the accepted [CLI RFC](../rfcs/accepted/CAPSULE_CLI_LIFECYCLE.md). That path
uses `init` / `resume` / `stop` / `encap` and wire v2 rather than the portable
application's contract-root identity.
