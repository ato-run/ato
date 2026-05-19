#!/usr/bin/env bash
# AODD pre-flight cleanup. Frees host resources commonly leaked by prior runs:
# - ato-postgres workers holding SysV shm
# - uvicorn / node workers holding ports
# - blinko server processes
# - port 1111 (Blinko default)
# - stale ato import child processes
#
# macOS and Linux compatible. No `xargs -r` (macOS xargs doesn't support it).
# No `pkill --` (macOS pkill doesn't accept `--`).
# Uses TERM only — no SIGKILL by default.
#
# NOTE: ato-desktop now offers "Clean up & Quit" in the quit confirmation
# dialog (Cmd+Q). The MCP tool `cleanup_host_resources` can also be used
# for programmatic cleanup. This script remains useful for pre-flight
# cleanup before starting the Desktop.
set -euo pipefail

echo "[aodd-cleanup] start $(date -u +%Y-%m-%dT%H:%M:%SZ)"

kill_port() {
  local port="$1"
  local pids
  pids="$(lsof -ti ":$port" 2>/dev/null || true)"
  if [ -n "$pids" ]; then
    echo "[aodd-cleanup] port $port: killing pids $pids"
    echo "$pids" | while read -r pid; do
      [ -n "$pid" ] && kill "$pid" 2>/dev/null || true
    done
  else
    echo "[aodd-cleanup] port $port: no listener"
  fi
}

kill_pattern() {
  local pattern="$1"
  local pids
  pids="$(pgrep -f "$pattern" 2>/dev/null || true)"
  if [ -n "$pids" ]; then
    echo "[aodd-cleanup] pattern '$pattern': killing pids $pids"
    echo "$pids" | while read -r pid; do
      [ -n "$pid" ] && kill "$pid" 2>/dev/null || true
    done
  else
    echo "[aodd-cleanup] pattern '$pattern': no match"
  fi
}

# ── port 1111 (Blinko default) ──
kill_port 1111

# ── stale postgres provider workers ──
kill_pattern "postgres.*ato"

# ── stale uvicorn workers ──
kill_pattern "uvicorn"

# ── stale blinko server processes ──
kill_pattern "blinko"

# ── stale Ato import child processes ──
kill_pattern "ato import github.com/blinkospace/blinko"

# ── allow processes a moment to exit ──
sleep 1

# ── best-effort hard kill for leftovers ──
kill_port 1111

echo "[aodd-cleanup] done $(date -u +%Y-%m-%dT%H:%M:%SZ)"
