#!/usr/bin/env bash
set -euo pipefail

if command -v aarch64-linux-gnu-gcc >/dev/null 2>&1; then
  exec aarch64-linux-gnu-gcc "$@"
fi

if command -v zig >/dev/null 2>&1; then
  args=()
  has_target=0

  for arg in "$@"; do
    case "$arg" in
      --target=aarch64-unknown-linux-gnu)
        args+=("--target=aarch64-linux-gnu")
        has_target=1
        ;;
      --target=aarch64-unknown-linux-musl)
        args+=("--target=aarch64-linux-musl")
        has_target=1
        ;;
      --target=*)
        args+=("$arg")
        has_target=1
        ;;
      *)
        args+=("$arg")
        ;;
    esac
  done

  if [[ "$has_target" -eq 0 ]]; then
    args=("--target=aarch64-linux-gnu" "${args[@]}")
  fi

  exec zig cc "${args[@]}"
fi

echo "error: missing C cross-compiler for aarch64-unknown-linux-gnu" >&2
echo "hint: install aarch64-linux-gnu-gcc or zig" >&2
exit 1
