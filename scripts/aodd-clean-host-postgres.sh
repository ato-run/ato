#!/bin/bash
# AODD validation — clean macOS arm64 host, no Homebrew PostgreSQL.
#
# Refs: #92 (clean-host gate), #119 (verified tool artifact downloader),
#       #120 (ato-postgres migration to ATO_TOOL_*).
#
# Run this on a fresh macOS arm64 host that has never installed
# Homebrew PostgreSQL. It produces a Usecase Receipt under
# claudedocs/aodd-receipts/<timestamp>/ that proves either:
#
#   - result: complete    — clean-host launch succeeded end-to-end
#   - result: blocked     — agent could not finish, transcript attached
#   - result: degraded    — finished but with retries / friction
#   - result: suspicious  — finished but accepted something risky
#
# The script automates the inspectable parts (host probe, log grep,
# state inventory) and prompts the human operator for the parts that
# require interactive Desktop use (entering SECRET_KEY / PG_PASSWORD,
# observing whether the db provider becomes ready, whether the app /
# web stages render).
#
# Usage:
#
#   bash scripts/aodd-clean-host-postgres.sh
#
# This script does not modify the host. It writes only inside the
# repo at $RECEIPT_DIR.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

TIMESTAMP="$(date -u +%Y%m%dT%H%M%SZ)"
RECEIPT_DIR="$REPO_ROOT/claudedocs/aodd-receipts/clean-host-postgres-$TIMESTAMP"
mkdir -p "$RECEIPT_DIR"

log() {
  printf '[aodd] %s\n' "$*"
}

step() {
  printf '\n[aodd] === %s ===\n' "$*"
}

receipt_step() {
  printf -- '- %s\n' "$*" >>"$RECEIPT_DIR/transcript.md"
}

# ────────────────────────────────────────────────────────────
# Phase A — host inventory (proves the host is "clean")
# ────────────────────────────────────────────────────────────

step "Phase A — host inventory"

OS_NAME="$(uname -s)"
ARCH="$(uname -m)"
log "uname:         $OS_NAME / $ARCH"

if [ "$OS_NAME" != "Darwin" ] || [ "$ARCH" != "arm64" ]; then
  log "WARNING: this script targets darwin-arm64; current host is $OS_NAME/$ARCH"
  log "         the postgresql tool artifact pin is darwin-aarch64 only."
fi

PG_ISREADY_ON_PATH="$(command -v pg_isready 2>/dev/null || true)"
PG_ISREADY_HOMEBREW="$( [ -e /opt/homebrew/bin/pg_isready ] && echo /opt/homebrew/bin/pg_isready || true )"
PG_ISREADY_USR_LOCAL="$( [ -e /usr/local/bin/pg_isready ] && echo /usr/local/bin/pg_isready || true )"
POSTGRES_HOMEBREW="$( [ -e /opt/homebrew/bin/postgres ] && echo /opt/homebrew/bin/postgres || true )"
POSTGRES_USR_LOCAL="$( [ -e /usr/local/bin/postgres ] && echo /usr/local/bin/postgres || true )"

log "which pg_isready:                 ${PG_ISREADY_ON_PATH:-(empty)}"
log "/opt/homebrew/bin/pg_isready:     ${PG_ISREADY_HOMEBREW:-(absent)}"
log "/usr/local/bin/pg_isready:        ${PG_ISREADY_USR_LOCAL:-(absent)}"
log "/opt/homebrew/bin/postgres:       ${POSTGRES_HOMEBREW:-(absent)}"
log "/usr/local/bin/postgres:          ${POSTGRES_USR_LOCAL:-(absent)}"

cat >"$RECEIPT_DIR/host-inventory.txt" <<EOF
host:                              $OS_NAME / $ARCH
which pg_isready:                  ${PG_ISREADY_ON_PATH:-(empty)}
/opt/homebrew/bin/pg_isready:      ${PG_ISREADY_HOMEBREW:-(absent)}
/usr/local/bin/pg_isready:         ${PG_ISREADY_USR_LOCAL:-(absent)}
/opt/homebrew/bin/postgres:        ${POSTGRES_HOMEBREW:-(absent)}
/usr/local/bin/postgres:           ${POSTGRES_USR_LOCAL:-(absent)}
EOF

CLEAN=true
if [ -n "$PG_ISREADY_ON_PATH" ] || [ -n "$PG_ISREADY_HOMEBREW" ] || [ -n "$PG_ISREADY_USR_LOCAL" ] \
   || [ -n "$POSTGRES_HOMEBREW" ] || [ -n "$POSTGRES_USR_LOCAL" ]; then
  CLEAN=false
fi

if $CLEAN; then
  log "host is clean: no host-installed Postgres detected."
else
  log "WARNING: host is NOT clean. The Phase 5 gate requires a host"
  log "         where 'which pg_isready' is empty AND no /opt/homebrew"
  log "         or /usr/local Postgres binaries exist. Current host"
  log "         shows leftover Postgres binaries (see above). The"
  log "         remaining phases will still exercise ATO_TOOL_*-based"
  log "         resolution, but the final 'clean-host' claim cannot"
  log "         be made from this run."
fi

# ────────────────────────────────────────────────────────────
# Phase B — start state of $ATO_HOME
# ────────────────────────────────────────────────────────────

step "Phase B — $ATO_HOME state before run"

ATO_HOME_DIR="${ATO_HOME:-$HOME/.ato}"
log "ATO_HOME = $ATO_HOME_DIR"

if [ -d "$ATO_HOME_DIR/store/tools" ]; then
  ls -la "$ATO_HOME_DIR/store/tools/" >"$RECEIPT_DIR/store-tools-before.txt" 2>&1 || true
  log "existing store/tools/ contents recorded to receipt."
else
  printf '(empty: %s/store/tools/ does not exist)\n' "$ATO_HOME_DIR" \
    >"$RECEIPT_DIR/store-tools-before.txt"
  log "store/tools/ does not exist yet — first-run download will populate it."
fi

# ────────────────────────────────────────────────────────────
# Phase C — operator instructions
# ────────────────────────────────────────────────────────────

step "Phase C — operator instructions for Desktop run"

cat <<EOF

You will now drive ato-desktop manually. The script cannot do this
because the production Desktop UX includes consent prompts and form
fields a real user would type into. AODD principle: don't bypass the
real surface.

Run on this host, in a separate terminal:

  open -a Ato

In the Desktop:

  1. Open the launcher
  2. Enter:    capsule://github.com/Koh0920/WasedaP2P
  3. Click "Start"
  4. When prompted, enter:
       SECRET_KEY = <any 32+ char value>
       PG_PASSWORD = <any password>
  5. Watch the run feed. The 'db' dependency should:
       - download postgresql 16.9.0 (~30 MB) on first run
       - reach 'ready' state via the native postgres probe
       - NOT spawn /opt/homebrew/bin/pg_isready
  6. Once db is ready, app/web stages should reach their own ready
     states.
  7. Open the served URL in a browser and confirm WasedaP2P loads.

EOF

read -r -p "[aodd] Press Enter when the Desktop run has completed (success or failure)... " _

# ────────────────────────────────────────────────────────────
# Phase D — post-run inventory
# ────────────────────────────────────────────────────────────

step "Phase D — post-run state of $ATO_HOME"

if [ -d "$ATO_HOME_DIR/store/tools" ]; then
  ls -la "$ATO_HOME_DIR/store/tools/" >"$RECEIPT_DIR/store-tools-after.txt" 2>&1 || true
  POSTGRES_DIR="$(ls -d "$ATO_HOME_DIR"/store/tools/postgresql-darwin-* 2>/dev/null | head -1 || true)"
  if [ -n "$POSTGRES_DIR" ]; then
    log "tool artifact installed: $POSTGRES_DIR"
    cat "$POSTGRES_DIR/.ato-tool-artifact.json" 2>/dev/null \
      >"$RECEIPT_DIR/postgresql-artifact-meta.json" || true
  else
    log "WARNING: no postgresql-darwin-* under store/tools/ — resolver did not run."
  fi
else
  log "WARNING: $ATO_HOME_DIR/store/tools/ does not exist — resolver did not run."
fi

# ────────────────────────────────────────────────────────────
# Phase E — log greps
# ────────────────────────────────────────────────────────────

step "Phase E — forbidden-path scan"

LOG_HITS_FILE="$RECEIPT_DIR/forbidden-path-hits.txt"
: >"$LOG_HITS_FILE"

scan_logs_for() {
  local pattern="$1"
  local label="$2"
  local out
  out="$(rg --no-heading --line-number "$pattern" \
    "$ATO_HOME_DIR/logs" "$ATO_HOME_DIR/runs" 2>/dev/null || true)"
  if [ -n "$out" ]; then
    {
      printf -- '\n--- %s ---\n' "$label"
      printf '%s\n' "$out"
    } >>"$LOG_HITS_FILE"
  fi
}

scan_logs_for '/opt/homebrew/bin/(pg_isready|initdb|postgres|pg_ctl|createdb)' '/opt/homebrew Postgres tools'
scan_logs_for '/usr/local/bin/(pg_isready|initdb|postgres|pg_ctl|createdb)'    '/usr/local Postgres tools'

if [ -s "$LOG_HITS_FILE" ]; then
  log "forbidden-path hits found — see $LOG_HITS_FILE"
  FORBIDDEN_HITS=true
else
  log "no /opt/homebrew or /usr/local Postgres-tool references in logs."
  FORBIDDEN_HITS=false
fi

# ────────────────────────────────────────────────────────────
# Phase F — operator-reported outcome
# ────────────────────────────────────────────────────────────

step "Phase F — receipt assembly"

cat <<EOF

Pick the AODD result based on what you actually observed in Desktop:

  complete    — db became ready, app/web rendered, no retries, no
                guesses, no /opt/homebrew leakage in logs.
  blocked     — db never became ready, or app/web never rendered.
  degraded    — finished but with retries, ambiguous errors, or
                doc-hunting along the way.
  suspicious  — finished but only by accepting something risky
                (unexpected permission, dangerous dialog, …).

EOF

read -r -p "[aodd] result (complete | blocked | degraded | suspicious): " RESULT
read -r -p "[aodd] one-line user-visible outcome: " USER_VISIBLE

cat >"$RECEIPT_DIR/receipt.yaml" <<EOF
usecase: "Clean macOS arm64 host launches WasedaP2P from ato-desktop without host PostgreSQL"
actor: "first-time user on a fresh macOS arm64 host"
goal: "WasedaP2P web UI reachable; db provider ready via Ato-managed Postgres binaries"

result: $RESULT

steps:
  - phase: A
    action: "host inventory"
    observation: "see host-inventory.txt; CLEAN=$CLEAN"
  - phase: B
    action: "$ATO_HOME state before run"
    observation: "see store-tools-before.txt"
  - phase: C
    action: "operator drove Desktop manually: capsule://github.com/Koh0920/WasedaP2P"
    observation: "see operator transcript"
  - phase: D
    action: "$ATO_HOME state after run"
    observation: "see store-tools-after.txt + postgresql-artifact-meta.json (if present)"
  - phase: E
    action: "forbidden-path scan over logs/ and runs/"
    observation: "FORBIDDEN_HITS=$FORBIDDEN_HITS; see forbidden-path-hits.txt"

agent_observation:
  user_visible_outcome: "$USER_VISIBLE"

evidence:
  - host-inventory.txt
  - store-tools-before.txt
  - store-tools-after.txt
  - postgresql-artifact-meta.json
  - forbidden-path-hits.txt

status: open
EOF

log "Receipt written to: $RECEIPT_DIR"
log ""
log "If result=complete and CLEAN=true and FORBIDDEN_HITS=false,"
log "the v0.5.x clean-host gate is satisfied. Update release notes:"
log ""
log "  post-#119/#120: clean-host Postgres provider support is"
log "  available for darwin-aarch64 after verified artifact validation."
log ""
