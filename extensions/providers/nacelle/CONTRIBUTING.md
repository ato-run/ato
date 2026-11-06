# Contributing to Nacelle

Nacelle is the OS-native sandbox and source-execution provider used by the Ato
runtime and CLI. It lives in the [ato-run/ato](https://github.com/ato-run/ato)
repository at `extensions/providers/nacelle`, and it is listed under `exclude`
in the root `Cargo.toml`, so it is built and tested from this directory rather
than as part of the root Cargo workspace.

## Before you start

Repository-wide rules are owned by the root guides; this page adds only what is
specific to Nacelle.

- [CONTRIBUTING.md](../../../CONTRIBUTING.md): base branches, pull requests,
  verification commands and documentation rules.
- [AGENTS.md](../../../AGENTS.md): Computation/Capsule invariants, component
  responsibilities and the Rust style used across the repository.
- [Nacelle security policy](SECURITY.md): sandbox boundary, threat model and
  vulnerability reporting.

Use [GitHub Discussions](https://github.com/ato-run/ato/discussions) for a design
question and [GitHub Issues](https://github.com/ato-run/ato/issues) for a
reproducible defect. Never report a security problem in a public issue; follow
[SECURITY.md](SECURITY.md).

## Toolchain

- `rust-toolchain` in this directory pins the `stable` channel. `Cargo.toml`
  declares `edition = "2024"` and `rust-version = "1.96"`.
- On Linux, `build.rs` compiles the program in `ebpf/` with
  `rustup run nightly cargo build ... -Z build-std` when host and target match,
  so a native Linux build of this crate needs a `nightly` toolchain in addition
  to `stable`; cross-compiled and non-Linux builds skip that step. If the eBPF
  build fails, `build.rs` only prints a `cargo:warning` and embeds an empty
  placeholder, so a successful `cargo build` does not by itself prove the eBPF
  program compiled.
- The eBPF program is its own crate in `ebpf/` (aya-based); build or inspect it
  there when you change it.

## Build, test and lint

```sh
cd extensions/providers/nacelle
cargo build
cargo test
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
```

- Unit tests live beside the code under `src/`; `cargo test --lib` is the fast
  loop.
- Integration tests are in `tests/`; `cargo test --tests` runs all of them and
  `cargo test --test <name>` runs one.
- `./scripts/test-samples.sh` checks the manifests under `samples/`. When a
  `nacelle` binary is on `PATH` it also runs the packaging steps; otherwise it
  validates manifests only.
- No pull-request workflow compiles this crate, because it is outside the root
  workspace. Run these checks locally and say in the pull request what you ran
  and what you could not verify on a real host.

## Architecture boundaries

- This is the provider that realizes a computation on a host: unpacking bundles
  and artifacts, OS-native isolation (filesystem and network; Seatbelt on macOS,
  Bubblewrap with Landlock on Linux), process launch and supervision, and socket
  activation.
- It does not decide policy. Signature verification, policy decisions and
  interactive UX belong on the host side ("Smart Build, Dumb Runtime"), and OS
  APIs are the host's responsibility (Host Bridge Pattern).
- Keep provider- and domain-specific action kinds out of the Semantic Core and
  the kernel; map provider behaviour onto the existing Protocols and Adapters.
- Sandbox profiles, the sensitive-path deny list and the network policy are
  security-relevant: change them with tests, and state in the pull request which
  boundary the change touches. See [SECURITY.md](SECURITY.md).

## Documentation and samples

- Provider documentation lives in `docs/`: the
  [engine interface contract](docs/ENGINE_INTERFACE_CONTRACT.md) and the ADRs in
  `docs/adr/`. Update them when behaviour changes.
- Samples live in `samples/`; a new sample needs a valid `capsule.toml` and a
  short README.

## Commits and pull requests

Follow the root guide for commit-message format, the no-`Co-Authored-By` rule,
one change per pull request, and the ban on publishing credentials, personal
data or unredacted application logs. When a change touches the sandbox boundary,
say which boundary it touches and what remains unverified.
