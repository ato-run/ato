# Contributing to Ato

Ato is experimental software. This file is the entry point for outside
contributors and is self-contained: everything it asks for is in this
repository. Start with [What is Ato?](README.md), the
[documentation map](docs/README.md) and [application
contracts](docs/concepts/application-contracts.md). Component notes live with
their code: [Desktop](apps/desktop/README.md),
[Nacelle](extensions/providers/nacelle/CONTRIBUTING.md) and the
[developer guides](docs/guides/README.md).

## Ask and report in the right place

| You have | Use |
|---|---|
| A design question, an idea, or "would this fit the model?" | [GitHub Discussions](https://github.com/ato-run/ato/discussions) |
| A reproducible bug | [GitHub Issues](https://github.com/ato-run/ato/issues) |
| A security vulnerability | the [Ato security policy](https://ato.run/legal/security/), never a public issue |
| A Nacelle sandbox or isolation question | the [Nacelle security policy](extensions/providers/nacelle/SECURITY.md) |

A design question does not need a patch: use a Discussion to test an idea
against the Computation, Capsule, Contract or verification model before writing
code. A bug report is actionable when it gives one expected behavior, the
commit, the platform, the exact command and what happened instead; attach the
verification receipt when the CLI wrote one.

## Read the accepted specification first

Check [accepted RFCs](docs/specifications/README.md) and the
[concepts](docs/concepts/computation.md) before implementing. Reuse existing
services, types, adapters and verifier authority instead of adding a parallel
implementation. A behavior that needs a contract and has none gets a draft RFC
under [docs/rfcs/draft/](docs/rfcs/draft/). When implementation and an accepted
RFC disagree, report **Detected mismatch** in the pull request; do not rewrite
the specification to match the code.

## Branches and pull requests

- `main` is the default branch. It is release-only: it is updated by release
  pull requests merged from the integration branch, not by feature work.
- Base new work on the integration branch, `stg`. `dev` and `nightly` remain
  for older integration and maintenance work; ask in a Discussion if the target
  is unclear instead of opening a pull request against `main`.
- One change per branch and pull request (`feat/...`, `fix/...`,
  `docs/...`). Do not mix unrelated refactors, reformatting or generated
  output.
- Branch protection is not currently enabled on `main` or `stg` on GitHub
  (verified 2026-11-04), so the rules above are conventions you follow
  deliberately rather than a gate that stops you.
- Merge access is not deployment authorization. Promotion to `main`, publishing
  a release and any deployment are separate reviewed steps.

## Verify your change

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo check --workspace
cargo test --workspace --no-fail-fast
```

Run the narrowest useful subset while iterating (`cargo test -p <crate> <name>`)
and the workspace set before review. CI adds the architecture, CLI and snapshot
checks; the workflows in [.github/workflows/](.github/workflows/) are
path-filtered, so read the checks that ran on your commit instead of assuming
they did.

`apps/desktop` (GPUI + Wry) and `extensions/providers/nacelle` sit outside the
root Cargo workspace and have their own build boundaries; follow their
instructions. Use a fresh isolated environment root for runtime tests rather
than reusing one; see [test isolation](docs/internal/testing-isolation.md).

Documentation-only changes need no build. Run the same link and fragment audit
as docs CI:

```sh
python3 scripts/docs-audit.py \
  --allow-historical-from docs/archive/2026/documentation-reorganization/link-audit.json \
  --output .tmp/docs-links.json
```

It resolves the repository root through `git`, uses no network and writes only
that JSON report; `.tmp/` is not committed. Docs pull requests also run the
[documentation acceptance workflow](.github/workflows/docs-ci.yml).

## Documentation changes

General user documentation is owned by [ato.run](https://ato.run/docs/); `docs/`
here is technical documentation for the runtime, CLI, specifications,
operations and archive. Ownership and Current/Draft/Archived rules are in
[docs/README.md](docs/README.md). Keep English as the default language for
current pages, and label translations and historical sources explicitly. Link
to the one canonical owner of a contract instead of copying it into several
guides. Do not rewrite a page under `docs/archive/`: archived material is kept
as historical evidence and as a compatibility target for links from old issues
and pull requests.

## What a pull request should contain

Commit coherent changes with the repository identity configured in your own
checkout and no `Co-Authored-By` trailer. The description should state what
changed, which Contract or specification governs the behavior, the commands you
actually ran and their result, and what is still unverified. Do not claim a
check you did not run.

Do not publish credentials, personal data, signed capability URLs or unredacted
application logs in a commit, a comment or a receipt committed to Git.
Operations and measurement records belong in `docs/ops/` or the pull request
body; work is tracked in GitHub Issues, not in a new Markdown roadmap file.
