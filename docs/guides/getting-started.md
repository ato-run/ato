# Getting started

Status: Current technical reference.

General installation and the user-facing CLI quickstart are owned by
[ato.run CLI quickstart](https://ato.run/docs/cli-quickstart/); this page is the
repository build and verification guide.

No account is required for the commands on this page except the optional
ato.run sign-in section, which describes unreleased work on `main`.

## Install the CLI

There are two channels. A published release is the supported install; building
from a reviewed commit is how you use changes that no published release
contains. A release install uses the cargo-dist artifacts published from a
tagged commit; a reviewed-commit build compiles the same CLI locally. Neither
channel requires an account.

### From a published release (v0.8.0)

The latest published release is [v0.8.0](https://github.com/ato-run/ato/releases/tag/v0.8.0)
(2026-07-28). Its release page provides the install channels and artifacts
below; nothing newer is published.

```sh
brew install ato-run/ato/cli       # macOS/Linux with Homebrew; the formula installs `ato`
```

On Windows, run the `.msi` attached to that release. Without Homebrew, use the
`cli-*.tar.xz` archive for your platform. Check the installed identity with
`ato version --json`; it must match the release tag. cargo-dist refuses a tag
whose crate version is behind an already-published one. There is no in-binary
self-update: update by reinstalling through the same channel.

A shell installer is not part of v0.8.0. The shell installer belongs to the
unreleased 0.8.1 distribution notes, so do not script an install from it yet.

### From a reviewed commit

Release 0.8.1 is unreleased. Build from a reviewed commit when you need the
changes on `main` that no published release contains. Install the Rust
toolchain declared in [rust-toolchain.toml](../../rust-toolchain.toml), then
install that exact commit:

```sh
cargo install --locked --git https://github.com/ato-run/ato.git \
  --rev <reviewed-commit> --bin ato ato-cli
ato version --json
```

`cargo install` writes the binary to `~/.cargo/bin`; use the in-tree build
below if you need to keep a release install untouched.

## Build from this checkout

Start in a checkout of this repository at a reviewed commit. These commands
build and use that checkout's CLI; deployment of matching services is not
implied. The examples below run `target/debug/ato`; substitute `ato` if you
installed the binary instead.

```sh
cargo build -p ato-cli --bin ato --locked
```

## Verify a supplied bundle

```sh
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

## Optional: ato.run sign-in

`ato login` opens a browser-approved device-code flow (PKCE) and stores the
credential per API origin in `$ATO_HOME/credentials.json` (0600 on Unix).
`ato upload` then uses that credential when `--auth-token` is not given.

```sh
ato login --api-url https://api.ato.run          # or --api-url https://staging.api.ato.run
ato upload <capsule-project> --api-url https://api.ato.run \
  --receipt object-upload-receipt.json
```

Review status: device-code login and the credential store are on `main` and in
the unreleased 0.8.1 line. No published release contains them (v0.8.0 is the
latest), so a release-channel install cannot use this path yet. They were
exercised against a local bridge in review. Hosted-environment sign-in and the
complete upload → private app path still need their own acceptance run, so treat
this section as preview rather than a certified end-to-end path. Do not promise
this path for a release until 0.8.1 is published.
