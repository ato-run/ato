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
    -A clippy::await_holding_lock \
    -A clippy::collapsible_if


echo "pre-commit quality checks passed"
