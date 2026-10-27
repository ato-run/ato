# Getting started

## Install the CLI

There are two supported ways to get `ato` today. Both produce the same
binary; the release artifacts are built by cargo-dist from a tagged commit.

From a checkout of this repository (works before a release is published):

```sh
cargo install --locked --git https://github.com/ato-run/ato.git \
  --rev <reviewed-commit> --bin ato ato-cli
ato version --json
```

From a published release (Homebrew on macOS/Linux, MSI on Windows, and the
shell installer attached to the release):

```sh
brew install ato-run/ato/ato        # or: brew upgrade ato
# Windows: run the .msi from the release page
# Linux/macOS without Homebrew: sh ato-cli-installer.sh
```

The version reported by `ato version` must match the release tag; cargo-dist
refuses a tag whose crate version is behind an already-published one. There is
no in-binary self-update: update by reinstalling through the same channel.

## Build from this checkout

Start in a checkout of this repository at a reviewed commit. Install the Rust
toolchain declared in [rust-toolchain.toml](../../rust-toolchain.toml). These
commands build and use that checkout's CLI; deployment of matching services is
not implied.

```sh
cargo build -p ato-cli --bin ato --locked
```

## Verify a supplied bundle

```sh
mkdir -p .tmp/getting-started
target/debug/ato run samples/interop-static.capsule --no-open \
  --verification-receipt .tmp/getting-started/receipt.json
```

## Sign in to ato.run

`ato login` opens a browser-approved device-code flow (PKCE) and stores the
credential per API origin in `$ATO_HOME/credentials.json` (0600 on Unix).
`ato upload` then uses that credential when `--auth-token` is not given.

```sh
ato login --api-url https://api.ato.run          # or --api-url https://staging.api.ato.run
ato upload <capsule-project> --api-url https://api.ato.run \
  --receipt object-upload-receipt.json
```

Review status: the device-code login and credential store shipped in 0.8.1;
they were exercised against a local bridge in review. Hosted-environment
sign-in and the complete upload → private app path still need their own
acceptance run, so treat this section as preview rather than a certified
end-to-end path.

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
