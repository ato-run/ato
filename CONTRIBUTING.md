# Contributing to Ato

Start with [What is Ato?](README.md), the [documentation map](docs/README.md),
and [Core Concepts](docs/concepts/application-contracts.md). Read
[AGENTS.md](AGENTS.md) and the applicable component instructions before
changing code. The workspace above this repository has additional instructions.

Use a separate feature/fix branch and an independent PR. Check the current
integration policy in the repository and owning workspace before selecting
a base; legacy `dev` work and the newer `stg` release flow must not be confused.
Check branch, HEAD, remote and worktree state, and preserve other tasks'
uncommitted work. Main promotion requires a separately reviewed release
candidate, and merge permission does not imply deployment permission.

Track work in [GitHub Issues](https://github.com/ato-run/ato/issues) (and GitHub
Projects when a board is used). Do not add a Markdown roadmap: the archived
[OPEN_ISSUES_ROADMAP](docs/archive/2026/legacy/OPEN_ISSUES_ROADMAP.md) is
historical evidence, and Formation stage state lives in
[FORMATION_ROADMAP](docs/rfcs/draft/FORMATION_ROADMAP.md).

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

General user documentation is owned by [ato.run](https://ato.run/docs/);
`docs/` here is technical documentation for the runtime, CLI, specifications,
operations and archive. Ownership and Current/Draft/Archived status rules are in
[docs/README.md](docs/README.md).

For documentation-only changes, verify links, commands, authority/language
labels and sensitive-data exposure. Application builds and deployments are
unnecessary. Keep English as the default current documentation language;
label Japanese translations and historical sources explicitly. Link to one
canonical owner rather than copying a contract into several guides.

Commit coherent changes with the configured repository identity and no
Co-Authored-By trailer. Describe final behavior, validation and remaining
limits in the PR. Do not publish credentials, personal data or unredacted
application logs. For provider-specific security details read [Nacelle security policy](extensions/providers/nacelle/SECURITY.md).
