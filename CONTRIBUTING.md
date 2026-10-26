# Contributing to Ato

Start with [What is Ato?](README.md), the [documentation map](docs/README.md),
and [Core Concepts](docs/concepts/application-contracts.md). Read
[AGENTS.md](AGENTS.md) and the applicable component instructions before
changing code. The workspace above this repository has additional instructions.

Normal development integrates through `dev`; use a separate feature/fix branch
and an independent PR. Check the current branch, HEAD, remote and worktree
state. Preserve other tasks' uncommitted work. Main promotion is a separate
release action, and merge permission does not imply deployment permission.

Check [accepted specs](docs/specifications/README.md) first. Reuse existing
services, types, adapters and verifier authority. An important behavior lacking
an accepted contract needs a draft RFC. If implementation conflicts with an
accepted RFC, report **Detected mismatch** instead of redefining the spec to
match the implementation.

Use the workspace [Cargo.toml](Cargo.toml), [toolchain](rust-toolchain.toml)
and [CI workflows](.github/workflows/) to choose checks. The root Rust baseline
is `cargo check --workspace`, `cargo test --workspace --no-fail-fast`,
`cargo fmt --all -- --check` and
`cargo clippy --all-targets --all-features -- -D warnings`. Desktop and Nacelle
have separate build boundaries. Use fresh isolated environment roots for
runtime tests; see [test isolation](docs/internal/testing-isolation.md).

For documentation-only changes, verify links, commands, authority/language
labels and sensitive-data exposure. Application builds and deployments are
unnecessary. Keep English as the default current documentation language;
label Japanese translations and historical sources explicitly. Link to one
canonical owner rather than copying a contract into several guides.

Commit coherent changes with the configured repository identity and no
Co-Authored-By trailer. Describe final behavior, validation and remaining
limits in the PR. Do not publish credentials, personal data or unredacted
application logs. For provider-specific security details read [Nacelle security policy](extensions/providers/nacelle/SECURITY.md).
