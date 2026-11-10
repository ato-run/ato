# CLI install verification — 2026-11-10

Status: Current evidence for the H1 install/documentation fix.

## Release and channels (checked against the published v0.8.2 release)

- Release: `v0.8.2` (2026-11-09). Assets include
  `ato-cli-aarch64-apple-darwin.tar.xz`, `ato-cli-x86_64-apple-darwin.tar.xz`,
  `ato-cli-aarch64-unknown-linux-gnu.tar.xz`,
  `ato-cli-x86_64-unknown-linux-gnu.tar.xz`,
  `ato-cli-x86_64-pc-windows-msvc.msi`, `ato-cli-x86_64-pc-windows-msvc.zip`,
  `ato-cli-installer.sh`, and `ato-cli.rb` (cargo-dist publish).
- Homebrew tap: `ato-run/homebrew-ato`; formulas `Formula/ato-cli.rb` (current
  release, class `AtoCli`), plus older `cli.rb` / `nacelle.rb`. The installed
  binary is `ato`. The formula name is **`ato-cli`**, not `ato`:
  `brew install ato-run/ato/ato-cli`.
- Shell installer: `https://github.com/ato-run/ato/releases/latest/download/ato-cli-installer.sh`
  resolves (HTTP 302 to the asset) and installs `ato`.
- Windows MSI: `ato-cli-x86_64-pc-windows-msvc.msi` (execution on Windows not
  performed by this check).

The guidance in `docs/guides/getting-started.md` previously named
`brew install ato-run/ato/ato`, which does not exist in the tap. Both READMEs
and the guide now name the real channel.

## `ato run --no-open` probe (static path)

Built from dev `a13b011e7` on macOS arm64 (`cargo build -p ato-cli --bin ato`).
The reported "static `--no-open` hangs" could not be reproduced:

| Command | Exit | Observation |
|---|---|---|
| `ato run samples/interop-static.capsule --no-open --verification-receipt .tmp/receipt.json` | 0 | receipt written; `fully_satisfied = true`; both observations `satisfied` |
| `ato run samples/datasette-cpu.capsule --no-open` | 1 | named refusal: declares 2 derivations; `--derivation` required (no hang) |
| `ato run samples/interop-multi-derivation.capsule --no-open` | 1 | same named refusal (no hang) |

The Windows/macOS differences of this path remain unverified from this host;
the probe matrix is the baseline to re-run on the other hosts in T1.

## Repository hygiene

Three tracked PR-body drafts at the repository root (`.tmp-consolidated-pr.md`,
`.tmp-d-pr.md`, `.tmp-e-pr.md`) were removed. `git grep` over the tree found no
references to any of them before removal.
