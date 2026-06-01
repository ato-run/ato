#!/usr/bin/env bash
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

echo "==> cargo fmt --all -- --check"
cargo fmt --all -- --check

echo "==> cargo check --workspace --all-targets"
cargo check --workspace --all-targets

echo "==> cargo clippy --workspace --all-targets -- -D warnings"
cargo clippy --workspace --all-targets -- -D warnings

echo "pre-commit quality checks passed"
