#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# Shared bash assertion library for e2e-host-isolation test cases.
# Source this file at the top of each run.sh:
#   source "$(dirname "$0")/../../harness/assert.sh"
#
# All functions exit 1 on failure, making them safe with set -euo pipefail.
# ─────────────────────────────────────────────────────────────────────────────

assert_equal() {
  local actual="$1" expected="$2" msg="${3:-assert_equal}"
  if [ "$actual" != "$expected" ]; then
    echo "❌ FAIL: $msg"
    echo "  expected: $expected"
    echo "  actual:   $actual"
    exit 1
  fi
  echo "✅ PASS: $msg"
}

assert_not_equal() {
  local actual="$1" forbidden="$2" msg="${3:-assert_not_equal}"
  if [ "$actual" = "$forbidden" ]; then
    echo "❌ FAIL: $msg"
    echo "  got forbidden value: $forbidden"
    exit 1
  fi
  echo "✅ PASS: $msg"
}

assert_contains() {
  local haystack="$1" needle="$2" msg="${3:-assert_contains}"
  if ! printf '%s' "$haystack" | grep -qF "$needle"; then
    echo "❌ FAIL: $msg"
    echo "  expected to contain: $needle"
    echo "  actual: $haystack"
    exit 1
  fi
  echo "✅ PASS: $msg"
}

assert_not_contains() {
  local haystack="$1" needle="$2" msg="${3:-assert_not_contains}"
  if printf '%s' "$haystack" | grep -qF "$needle"; then
    echo "❌ FAIL: $msg"
    echo "  must not contain: $needle"
    echo "  actual: $haystack"
    exit 1
  fi
  echo "✅ PASS: $msg"
}

assert_file_exists() {
  local path="$1" msg="${2:-file must exist}"
  if [ ! -e "$path" ]; then
    echo "❌ FAIL: $msg"
    echo "  path: $path"
    exit 1
  fi
  echo "✅ PASS: $msg ($path)"
}

assert_dir_empty() {
  local dir="$1" msg="${2:-directory must be empty}"
  local entries
  entries=$(ls -A "$dir" 2>/dev/null || true)
  if [ -n "$entries" ]; then
    echo "❌ FAIL: $msg"
    echo "  directory: $dir"
    echo "  found: $entries"
    exit 1
  fi
  echo "✅ PASS: $msg ($dir)"
}
