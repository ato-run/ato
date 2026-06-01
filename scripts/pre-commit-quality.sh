#!/usr/bin/env bash
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"

echo "==> [root] cargo fmt --all -- --check"
cargo fmt --all -- --check

echo "==> [root] cargo check --workspace --all-targets"
cargo check --workspace --all-targets

echo "==> [root] cargo clippy --workspace --all-targets -- -D warnings"
cargo clippy --workspace --all-targets -- -D warnings \
    -A unused_unsafe \
    -A clippy::await_holding_lock \
    -A clippy::collapsible_if

# ato-desktop is an independent workspace (it carries its own empty
# `[workspace]` table), so the `--workspace` runs above do NOT cover it.
# Gate it separately.
#
# Only fmt + build (check) are enforced here. `clippy -D warnings` is
# intentionally NOT applied to ato-desktop yet: the crate carries ~245
# pre-existing dead-code / clippy warnings that are out of scope for the
# edition-2024 migration. Tightening this to -D warnings requires clearing
# those first. Window/WebView code lives under the bin target, so we check
# `--bin ato-desktop` (this also compiles the lib it depends on).
echo "==> [ato-desktop] cargo fmt --all -- --check"
( cd crates/ato-desktop && cargo fmt --all -- --check )

echo "==> [ato-desktop] cargo check --bin ato-desktop"
( cd crates/ato-desktop && cargo check --bin ato-desktop )

echo "pre-commit quality checks passed"
