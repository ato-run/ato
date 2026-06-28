#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# Cross-runner native-inference smoke (#754 gate)
# ─────────────────────────────────────────────────────────────────────────────
# Verifies the FULL Connected Runner native-inference path end-to-end:
#
#   POST /v1/runs (external-runner)               ── operator session
#     → ato-api mints a run_capsule lease, runtime="native-inference" (ato-api #149)
#     → dispatch is gated on the runner advertising `native-inference`
#       (a 409 runner_capability_required here = the runner does NOT advertise it)
#   → runner claims the lease, dispatches `ato run <run_ref> -y` WITHOUT --sandbox (ato #763)
#   → run reaches ready  →  /health 200  →  /v1/chat/completions works
#   → ato stop / cleanup succeeds
#
# Two evidence tiers:
#   • API tier (always): dispatch + lifecycle + ready + completion + stop.
#       This alone is a strong functional proof — a native-inference capsule forced
#       into --sandbox would FAIL, so a GREEN ready+completion means the host
#       dispatch path ran. Dispatch SUCCESS is also the only operator-API signal
#       that the runner advertises `native-inference` (it is NOT on GET /v1/runners).
#   • Runner-SSH tier (optional, recommended): the DIRECT proof — `ato ps --json`
#       runtime=host, /proc/<pid>/cmdline shows `ato run <ref> -y` with NO
#       `--sandbox`, and the engine log shows llama-server / Vulkan. lease.command.
#       runtime is NOT exposed by the operator API (runner-fetched only), so the
#       direct argv/runtime proof requires runner-host access.
#
# This script makes NO production changes. It dispatches one run and stops it.
# See docs/ops/connected-runner-native-inference-smoke.md for prerequisites.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail

# ── Inputs (env) ─────────────────────────────────────────────────────────────
API_BASE="${ATO_API_BASE:-https://staging.api.ato.run}"   # prod: https://api.ato.run
SESSION="${ATO_SESSION:-}"                                 # operator better-auth session token (REQUIRED)
RUNNER_ID="${ATO_RUNNER_ID:-}"                             # target Connected Runner id (REQUIRED)
CAPSULE_REF="${ATO_CAPSULE_REF:-community/local-llm-chat}" # native-inference capsule (must be registered — #754)
RUNNER_SSH="${ATO_RUNNER_SSH:-}"                           # optional "user@host" for the direct argv/log proof
RUNNER_SSH_KEY="${ATO_RUNNER_SSH_KEY:-}"                   # optional ssh -i key path
PROMPT="${ATO_PROMPT:-Reply with the single word: ready}"
READY_TIMEOUT="${ATO_READY_TIMEOUT:-300}"                  # seconds to wait for status=ready
STOP_TIMEOUT="${ATO_STOP_TIMEOUT:-120}"
EVIDENCE_DIR="${ATO_EVIDENCE_DIR:-./smoke-evidence}"

# ── Colours + counters ───────────────────────────────────────────────────────
RED=$'\033[0;31m'; GREEN=$'\033[0;32m'; YELLOW=$'\033[1;33m'; BLUE=$'\033[0;34m'; NC=$'\033[0m'
PASS=0; FAIL=0; WARN=0
section() { echo; echo "${BLUE}━━━ $* ━━━${NC}"; }
ok()   { echo "${GREEN}✓${NC} $*"; PASS=$((PASS+1)); }
bad()  { echo "${RED}✗${NC} $*"; FAIL=$((FAIL+1)); }
warn() { echo "${YELLOW}!${NC} $*"; WARN=$((WARN+1)); }

# ── Redaction — scrub secrets from any captured text before it is printed/saved.
# perl with \Q…\E matches the session token LITERALLY: a sed regex mishandles the
# +, /, . that occur in real better-auth tokens and would silently fail to redact.
# The token is passed via env (never interpolated into the perl source).
redact() {
  ATO_SESSION_REDACT="$SESSION" perl -pe '
    BEGIN { $s = $ENV{ATO_SESSION_REDACT} // ""; }
    s/\Q$s\E/[REDACTED_SESSION]/g if length $s;
    s/(ato_rnr_)[\w.\-]+/${1}[REDACTED]/g;
    s/(ghp_|github_pat_)[\w.\-]+/${1}[REDACTED]/g;
    s/(better-auth[^=\s]*=)[^;"\s]+/${1}[REDACTED]/g;
    s/([?&](?:token|session|key|secret|sig|code|jwt)=)[^&"\s]+/${1}[REDACTED]/gi;
    s/("?(?:authorization|cookie|token|secret|password|api[_-]?key)"?\s*[:=]\s*"?)(?:Bearer\s+)?[^",}\s]+/${1}[REDACTED]/gi;
  '
}

COOKIE="better-auth.session_token=${SESSION}; __Secure-better-auth.session_token=${SESSION}"
# api <METHOD> <path> [json-body]  → echoes "<http_code>\n<body>"
api() {
  local method="$1" path="$2" body="${3:-}"
  local args=(-s -w $'\n%{http_code}' --connect-timeout 8 --max-time 30 -X "$method" \
              -H "Content-Type: application/json" -H "Cookie: ${COOKIE}")
  [ -n "$body" ] && args+=(-d "$body")
  curl "${args[@]}" "${API_BASE}${path}" 2>/dev/null
}
http_code() { tail -n1 <<<"$1"; }
http_body() { sed '$d' <<<"$1"; }

rssh() {  # run a command on the runner host (if RUNNER_SSH set)
  local key=(); [ -n "$RUNNER_SSH_KEY" ] && key=(-i "$RUNNER_SSH_KEY")
  ssh "${key[@]}" -o ConnectTimeout=12 -o StrictHostKeyChecking=accept-new -o IdentitiesOnly=yes "$RUNNER_SSH" "$@" 2>/dev/null
}

mkdir -p "$EVIDENCE_DIR"
SUMMARY="$EVIDENCE_DIR/summary.txt"
: > "$SUMMARY"
note() { echo "$*" | redact | tee -a "$SUMMARY" >/dev/null; }

echo "${BLUE}Cross-runner native-inference smoke${NC}  (api=${API_BASE})"
note "api_base: ${API_BASE}"
note "capsule_ref: ${CAPSULE_REF}"
note "runner_id: ${RUNNER_ID}"
note "runner_ssh: $([ -n "$RUNNER_SSH" ] && echo yes || echo 'no (API-tier only)')"

# ── 0. Preflight ─────────────────────────────────────────────────────────────
section "0. Preflight"
for c in curl jq perl; do command -v "$c" >/dev/null || { bad "missing required command: $c"; exit 2; }; done
ok "curl + jq + perl present"
[ -n "$SESSION" ]   || { bad "ATO_SESSION (operator session token) is required"; exit 2; }
[ -n "$RUNNER_ID" ] || { bad "ATO_RUNNER_ID (target Connected Runner) is required"; exit 2; }
hc=$(curl -s -o /dev/null -w '%{http_code}' --max-time 10 "${API_BASE}/health")
[ "$hc" = "200" ] && ok "API /health = 200" || { bad "API /health = ${hc}"; exit 2; }
sess=$(api GET /api/auth/session)
who=$(http_body "$sess" | jq -r '.user.email // empty' 2>/dev/null)
[ -n "$who" ] && ok "session valid (operator: ${who})" || { bad "session invalid/expired"; exit 2; }

# ── 1. Runner preflight ──────────────────────────────────────────────────────
section "1. Runner ${RUNNER_ID}"
runners=$(api GET /v1/runners)
rj=$(http_body "$runners" | jq -c --arg id "$RUNNER_ID" '.runners[]? | select(.id==$id)' 2>/dev/null)
if [ -z "$rj" ]; then
  bad "runner ${RUNNER_ID} not found / not owned by ${who} — enroll a #763-bearing 0.7.x runner first"
  exit 3
fi
online=$(jq -r '.online' <<<"$rj"); ver=$(jq -r '.agent_version // "?"' <<<"$rj")
note "runner_agent_version: ${ver}"; note "runner_online: ${online}"
[ "$online" = "true" ] && ok "runner online" || warn "runner not online (status/last_seen stale) — proceed anyway"
ok "runner agent_version=${ver} os=$(jq -r '.os' <<<"$rj") arch=$(jq -r '.arch' <<<"$rj")"
warn "native-inference capability is NOT on GET /v1/runners (serializeRunner omits supported_lease_kinds) — confirmed indirectly by the dispatch below"

# ── 2. Dispatch (also the indirect capability proof) ─────────────────────────
section "2. Dispatch ${CAPSULE_REF} → runner ${RUNNER_ID}"
disp=$(api POST /v1/runs "$(jq -nc --arg r "$RUNNER_ID" --arg a "$CAPSULE_REF" \
  '{placement:"external-runner", runner_id:$r, app_id:$a, client:{os:"web", desktop_companion_available:false}}')")
code=$(http_code "$disp"); body=$(http_body "$disp")
err=$(jq -r '.error // empty' <<<"$body" 2>/dev/null)
if [ "$code" = "409" ] && [ "$err" = "runner_capability_required" ]; then
  bad "dispatch → 409 runner_capability_required: runner ${RUNNER_ID} does NOT advertise native-inference (needs a #763 runner that passes \`ato doctor native-inference\`)"
  http_body "$disp" | redact | tee -a "$SUMMARY"; exit 4
fi
if [ "$code" != "201" ]; then
  bad "dispatch failed: HTTP ${code} error=${err:-?}"
  http_body "$disp" | redact | tee -a "$SUMMARY"; exit 4
fi
RUN_ID=$(jq -r '.id' <<<"$body"); LEASE_ID=$(jq -r '.lease_id // empty' <<<"$body")
ok "dispatch 201 → run ${RUN_ID} (lease ${LEASE_ID:-?})"
ok "INDIRECT capability proof: dispatch accepted ⇒ runner advertises native-inference (ato-api #149 gate)"
note "run_id: ${RUN_ID}"; note "lease_id: ${LEASE_ID}"
echo "$body" | redact > "$EVIDENCE_DIR/dispatch.json"

# ── 3. Runner-side DIRECT proof (optional, requires SSH) ──────────────────────
section "3. Runner-side proof (--sandbox absent / runtime=host / engine log)"
if [ -z "$RUNNER_SSH" ]; then
  warn "ATO_RUNNER_SSH not set — skipping the direct argv proof (API tier still proves the functional path). Set ATO_RUNNER_SSH=user@host for the --sandbox-absent evidence."
else
  ok "ssh target: ${RUNNER_SSH}"
  # Wait for the runner to claim + spawn the workload.
  cmdline=""; runtime_label=""; logp=""; wpid=""
  for _ in $(seq 1 30); do
    psj=$(rssh 'ato ps --json 2>/dev/null || $HOME/.cargo/bin/ato ps --json 2>/dev/null')
    row=$(jq -c --arg ref "$CAPSULE_REF" '.[]? | select((.name//"")|test($ref)) // empty' <<<"$psj" 2>/dev/null | head -1)
    [ -z "$row" ] && row=$(jq -c '.[]? | select(.runtime=="host")' <<<"$psj" 2>/dev/null | tail -1)
    if [ -n "$row" ]; then
      runtime_label=$(jq -r '.runtime' <<<"$row"); logp=$(jq -r '.log_path // empty' <<<"$row")
      wpid=$(jq -r '.workload_pid // .pid' <<<"$row")
      cmdline=$(rssh "tr '\\0' ' ' < /proc/${wpid}/cmdline 2>/dev/null || ps -p ${wpid} -o command= 2>/dev/null")
      [ -n "$cmdline" ] && break
    fi
    sleep 4
  done
  if [ -n "$cmdline" ]; then
    note "runner_argv: ${cmdline}"
    note "runtime_label: ${runtime_label}"
    echo "$cmdline" | redact > "$EVIDENCE_DIR/runner-argv.txt"
    if grep -q -- '--sandbox' <<<"$cmdline"; then
      bad "runner argv CONTAINS --sandbox → native-inference was sandboxed (regression vs #763): ${cmdline}"
    else
      ok "argv has NO --sandbox (native-inference dispatched host): ${cmdline}"
    fi
    [ "$runtime_label" = "host" ] && ok "ato ps runtime=host" || warn "ato ps runtime=${runtime_label:-?} (expected host)"
    if [ -n "$logp" ]; then
      eng=$(rssh "grep -iE 'vulkan|ggml|llama|server is listening|RTX|listening on' '${logp}' 2>/dev/null | head -3")
      [ -n "$eng" ] && { ok "engine log shows llama.cpp/Vulkan"; echo "$eng" | redact | sed 's/^/    /'; echo "$eng" | redact > "$EVIDENCE_DIR/engine-log.txt"; } \
                    || warn "no llama/Vulkan line found in ${logp} yet"
    fi
  else
    warn "could not capture runner argv via SSH (workload not visible in \`ato ps\` yet, or ATO_HOME differs)"
  fi
fi

# ── 4. Ready → /health → completion ──────────────────────────────────────────
section "4. Ready → /health → completion"
status=""; ready_url=""; deadline=$(( $(date +%s 2>/dev/null || echo 0) + READY_TIMEOUT ))
while :; do
  rj=$(http_body "$(api GET "/v1/runs/${RUN_ID}")")
  status=$(jq -r '.status' <<<"$rj"); ready_url=$(jq -r '.ready_url // empty' <<<"$rj")
  echo -ne "  status=${status} ready_url=$([ -n "$ready_url" ] && echo set || echo -)        \r"
  case "$status" in
    ready) [ -n "$ready_url" ] && break ;;
    failed|stopped|cancelled) break ;;
  esac
  [ "$(date +%s 2>/dev/null || echo 0)" -ge "$deadline" ] && break
  sleep 5
done
echo
note "final_status: ${status}"; note "ready_url: ${ready_url:-none}"
if [ "$status" = "ready" ]; then
  ok "run reached ready"
else
  bad "run did not reach ready (status=${status})"
  diag=$(http_body "$(api GET "/v1/diagnostics/reports?run_id=${RUN_ID}")")
  echo "$diag" | redact | jq -r '.reports[]? | "  diag: \(.error_class // "?") [\(.status // "?")] report=\(.report_id // .id // "?")"' 2>/dev/null | tee -a "$SUMMARY"
fi
if [ -n "$ready_url" ]; then
  h=$(curl -s -o /dev/null -w '%{http_code}' --max-time 15 -H "Cookie: ${COOKIE}" "${ready_url%/}/health")
  [ "$h" = "200" ] && ok "/health = 200" || warn "/health = ${h} (capsule may expose a different health path)"
  comp=$(curl -s --max-time 60 -H "Content-Type: application/json" -H "Cookie: ${COOKIE}" \
    -d "$(jq -nc --arg p "$PROMPT" '{model:"local",messages:[{role:"user",content:$p}],max_tokens:16}')" \
    "${ready_url%/}/v1/chat/completions" 2>/dev/null)
  txt=$(jq -r '.choices[0].message.content // empty' <<<"$comp" 2>/dev/null)
  if [ -n "$txt" ]; then ok "completion: $(echo "$txt" | tr '\n' ' ' | cut -c1-80)"; note "completion: ${txt}"; echo "$comp" | redact > "$EVIDENCE_DIR/completion.json"
  else warn "no completion text (endpoint/path may differ for this capsule)"; fi
fi

# ── 5. Stop / cleanup ────────────────────────────────────────────────────────
section "5. Stop / cleanup"
st=$(api POST "/v1/runs/${RUN_ID}/stop")
ok "stop requested (HTTP $(http_code "$st"))"
deadline=$(( $(date +%s 2>/dev/null || echo 0) + STOP_TIMEOUT ))
while :; do
  s=$(jq -r '.status' <<<"$(http_body "$(api GET "/v1/runs/${RUN_ID}")")")
  echo -ne "  status=${s}        \r"
  case "$s" in stopped|cancelled|failed) break ;; esac
  [ "$(date +%s 2>/dev/null || echo 0)" -ge "$deadline" ] && break
  sleep 4
done
echo; note "stop_final_status: ${s:-?}"
case "${s:-}" in stopped|cancelled) ok "run ${s} — cleanup acked" ;; *) warn "run did not reach stopped (status=${s:-?}) within ${STOP_TIMEOUT}s" ;; esac
if [ -n "$RUNNER_SSH" ] && [ -n "${wpid:-}" ]; then
  rssh "kill -0 ${wpid} 2>/dev/null" && warn "workload pid ${wpid} still alive on runner" || ok "workload pid gone on runner"
fi

# ── Verdict ──────────────────────────────────────────────────────────────────
section "Verdict"
note "result: PASS=${PASS} FAIL=${FAIL} WARN=${WARN}"
echo "Evidence (redacted): ${EVIDENCE_DIR}/  →  summary.txt, dispatch.json$([ -n "$RUNNER_SSH" ] && echo ', runner-argv.txt, engine-log.txt'), completion.json"
if [ "$FAIL" -eq 0 ] && [ "$status" = "ready" ]; then
  echo "${GREEN}GREEN — native-inference ran on the Connected Runner via the #149/#763 contract.${NC}"
  exit 0
else
  echo "${RED}NOT GREEN — see failures above (${FAIL} fail, ${WARN} warn).${NC}"
  exit 1
fi
