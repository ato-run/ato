# Getting started

Start in a checkout of this repository at a reviewed commit. Install the Rust
toolchain declared in [rust-toolchain.toml](../../rust-toolchain.toml). These
commands build and use that checkout's CLI; deployment of matching services is
not implied.

```sh
cargo build -p ato-cli --bin ato --locked
mkdir -p .tmp/getting-started
target/debug/ato run samples/interop-static.capsule --no-open \
  --verification-receipt .tmp/getting-started/receipt.json
```

The [supplied bundle](../../samples/interop-static.capsule) is the static
interoperability fixture. `run` validates its object closure, realizes the
declared route and verifies its captured source identity and HTTP body digest.
`--no-open` verifies and exits without opening a browser.

Inspect `receipt.json`: successful verification names the bundle transport
hash, K, D, target and each observation, with `fully_satisfied = true`. This
exact example was executed on macOS during the acceptance review: both
observations were satisfied. That result does not certify every platform or
route. Preserve failure output rather than recapturing K from a failed Run.

## Package a new application

The current `pack` command consumes `schema = "ato.capsule/2"`. Use the
[authored process example](../../samples/portable-multi-process-authored/capsule.toml):

```sh
target/debug/ato pack samples/portable-multi-process-authored \
  -o .tmp/getting-started/application.capsule
```

Packaging this example was also verified during review. Executing it requires
the declared Python runtime and a supported process sandbox; it declares two
Ds, so `run` additionally needs an explicit `--derivation <declared-ref>`.
Continue with the [Formation guide](formation.md) before that execution.
The older `interop-static-k` source uses `ato.capsule/1` and is not accepted
by the current `pack` frontend; use its supplied bundle for the first example.

For an application requiring Python, OCI, GPU, state or external Bindings,
read [Formation](formation.md) and [Runner / Hosting](runner-hosting.md) before
execution. Host capability and permissions are separate admission checks.

For the computation-root lifecycle, continue with [Local lifecycle](../run.md)
and the accepted [CLI RFC](../rfcs/accepted/CAPSULE_CLI_LIFECYCLE.md). That path
uses `init` / `resume` / `stop` / `encap` and wire v2 rather than the portable
application's contract-root identity.
